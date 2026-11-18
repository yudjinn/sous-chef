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
  layer's `BBFILES` glob matches (or only an inactive `BBFILES_DYNAMIC` entry does, or a
  static `BBMASK` masks) is no recipe, no provider and no bbappend, and a tool that shows one
  says so in a note; a data file (a `.conf` / `.inc` / `.bbclass` a recipe
  installs, bitbake-semantics.md "Data files") is shown by `item` with a note and left out of
  `search`, file listings and lint.
- Recipe arguments accept `pn`, `pn_pv` or a pointer. A name that is no recipe's exact `pn`
  is an error listing up to 20 recipe names that contain it. In `recipe`, `var`,
  `overrides`, `deps`, `item`, `search` and the `scaffold` `bbappend` kind a bare `pn`
  names the file BitBake would use, through provider selection (`PREFERRED_VERSION`,
  `DEFAULT_PREFERENCE`, layer priority, version; see "Providers and versions" in
  bitbake-semantics.md).
- Class-extended variants (since w10; bitbake-semantics.md "Class-extended variants"): a
  name that is no recipe but has the form of a variant of one whose `BBCLASSEXTEND` lists the
  class (`<pn>-native`, `nativesdk-<pn>`, `<variant>-<pn>` for `multilib:<variant>`) names
  that variant: `var`, `overrides`, `deps` and the evaluated `recipe` views evaluate it as
  BitBake makes it (the base recipe and its bbappends, then the class, finalised; `FILE` is
  the base file), with a target note saying so and the values the class-extension handlers
  filter flagged; `recipe` shows a `variant` row (its evaluation target
  `virtual:<word>:<layer>:<path>`, which every evaluating tool also accepts as `recipe`);
  `item`, `search` and the `bbappend` generator work on the base file; `deps` resolves a
  variant as a provider of its name and (since w11) of the `PROVIDES` its class rewrites
  (`virtual/x-native` for a native variant of a recipe providing `virtual/x`), beside any
  real recipe of the same name (provider and version selection choose among them as
  BitBake does). Lint's target is the base recipe (an error says so).
- Provider gaps, said in the output (bitbake-semantics.md "What the static evaluator cannot
  do"): a `BBMASK` the evaluator cannot apply (python, undefined variables, a
  regular expression Rust cannot compile) gives `note: BBMASK is set but sous-chef does not
  apply it (...)` in every evaluated view; a recipe that `SKIP_RECIPE[<pn>]`,
  `COMPATIBLE_MACHINE`, `COMPATIBLE_HOST`, `INCOMPATIBLE_LICENSE`, (since w11)
  `features_check.bbclass` (`REQUIRED_` / `CONFLICT_` / `ANY_OF_DISTRO_FEATURES` and the
  `MACHINE`, `COMBINED`, `IMAGE`, `TUNE` ones) or OE-core's `RecipeParsed` handler (it
  provides a `virtual/` name whose `PREFERRED_PROVIDER_` names another recipe) makes BitBake skip,
  when that is decided from static values, gives `note: BitBake would skip this recipe:
  ...`; when the check fails on what is known but values that are not known (python,
  undefined variables, a word only possibly present because an override decision on it is
  not certain) could still decide it, `note: BitBake may skip this recipe: ...`
  names them (`COMPATIBLE_MACHINE = "^qemu.*$" matches none of the known machine overrides
  (none is known); 1 part of MACHINEOVERRIDES is not evaluated`); a check that passes on
  known values (a match on a known `MACHINEOVERRIDES` entry) says nothing. An evaluation
  for an explicit `machine` or `distro` whose `BBMASK` masks other files than the index's
  (made with the default machine and distro) says which: `note: BBMASK for this machine and
  distro differs from the one the index applied (...): it would also mask 1 file:
  meta-mine:recipes-app/app/app_0.1.bb; ...`.
- `machine` and `distro`, where a tool takes them, default to what the evaluated
  configuration sets (`local.conf`, and OE configuration fragments such as bitbake-setup's
  `OE_FRAGMENTS` in `conf/toolcfg.conf`). In a kas project whose build directory does not
  exist yet, the kas files' `machine` / `distro` (else kas's defaults; a `KAS_MACHINE` /
  `KAS_DISTRO` in the environment sous-chef was started in wins, as in kas) are used as
  `MACHINE ??=` / `DISTRO ??=` where `bitbake.conf` includes `conf/local.conf`, as kas
  writes them, with a note naming where they came from (`note: MACHINE = "board1" from the
  kas file kas/board.yaml:5 (kas project without a build directory; ...)`, from
  `tests/eval_w8.rs`). When nothing sets `MACHINE`, every evaluated view ends with
  `note: no MACHINE is set (...)`: machine-specific values are not applied.
- **Multiconfig** (since x5; bitbake-semantics.md "Multiconfig"). `var`, `overrides`, `deps`
  and `recipe` take `mc` (a name in `BBMULTICONFIG`), and also a recipe written
  `mc:<name>:<recipe>` (or the old `multiconfig:<name>:<recipe>`; both forms naming
  different configurations is an error). The configuration is replayed again with
  `BB_CURRENT_MC` set and `conf/multiconfig/<name>.conf` read where OE-core's `bitbake.conf`
  requires it (after `local.conf`, before the machine conf), so the multiconfig's
  `MACHINE`, `DISTRO`, `TMPDIR`, `PREFERRED_PROVIDER_*` and machine overrides apply;
  `machine` / `distro` arguments still override on top. Every answer in a multiconfig
  carries one note, `note: multiconfig <name>: the configuration parsed again with
  BB_CURRENT_MC = "<name>", conf/multiconfig/<name>.conf (<pointer>) read where
  conf/bitbake.conf requires it`; an answer for the default configuration (no `mc`, or an
  empty one) has none and is exactly as before. An unknown name is an error listing the
  known ones (`multiconfig nope; known: tiny, musl`), and a listed name whose file is not
  found is an error as in BitBake. `status` lists the configurations. Only the default
  configuration is used by: lint (recipe-scope rules evaluate in it), the index's `BBMASK`
  marks (which files are collected, so the candidates of provider selection in every
  configuration), the audit report, `item`, `search`, `class`, `layers`, `slice` and
  `scaffold`, and the authoritative `var` (an error with `mc`; use `bitbake op=env
  target=mc:<name>:<recipe>`).

- **Progress.** A call whose request carries `_meta.progressToken` gets MCP
  `notifications/progress` while a long operation runs, before its result and never after
  it: `bitbake` with `wait` (and while a request waits behind a read-only job), the
  authoritative `var` (waiting for its `bitbake -e`), `lint` (since w11: the files checked,
  `progress` = files done, `total` = files planned, message `lint: N of M files`), and
  `audit` (one step per stage: refreshing the index, finding layers and git facts, lint,
  lint done, parse problems; during the lint stage the files as for `lint`, with
  `total`, message `audit: linting, N of M files`). `progress` increases with every
  notification (seconds since the call began for the waits, a step count for the `audit`
  stages, the file count for lint); only the lint updates have a `total`; `message` says
  what is happening. At most four a second: intermediate updates are dropped, and the
  newest pending one is sent before the result. A request without a token gets none.

The first six are always loaded. The rest are deferred (short descriptions, loaded on demand).

**Context cost (since x7).** Every tool definition a client loads stays in the agent's
context on every turn. Claude Code's tool search (on by default) defers every MCP tool,
keeping only its name and the server instructions in context until the agent searches
for it, unless the tool's `_meta` has `"anthropic/alwaysLoad": true`
(code.claude.com/docs/en/mcp, "Mark a tool to load upfront or stay deferred"): the six
always-loaded tools carry it, the deferred ones carry no `_meta`. Without tool search
(tool search off, an older model, or a session whose `--tools` leaves `ToolSearch` out, as
the benchmark's did) every definition loads upfront, so all are kept short: the
descriptions here say what an agent needs to choose and call a tool, and the input schema
is sent compacted (`mcp::compact_schema`: no `$schema` or `title`, enums written in place
without per-value docs, `Option<T>` as `T`, no integer `format`/`minimum`, the shared
`budget_tokens` / `format` docs left out because the server instructions give them). The
long form is this document and `sous-chef://help`, which is generated from the full schema
and lists every enum value with its doc. Measured with `cargo test --test tools_budget --
--nocapture`: `tools/list` was 31,539 bytes (about 7,900 tokens at 4 bytes a token) for the
18 tools before x7 and is 14,102 bytes (about 3,500 tokens) after; the test fails above
15,000 bytes, above 800 bytes of server instructions, or above 3,000 bytes of the skill.

## Always loaded

### `recipe`
Resolve a recipe and show its shape.

| Arg | Type | Notes |
|---|---|---|
| `name` | string | required: a `pn`, a `pn_pv`, or a pointer to a `.bb` or a `.bbappend` (a bbappend pointer means the recipe it applies to) |
| `version` | string? | default: the version BitBake would use (provider selection for the default machine and distro); `%` matches any rest |
| `show` | `"summary"` \| `"tree"` \| `"outline"` \| `"sources"` \| `"tasks"` \| `"packages"` \| `"cve"` | default `summary` |
| `mc` | string? | multiconfig, a name in `BBMULTICONFIG` (common section, "Multiconfig"; also `name` written `mc:<name>:<recipe>`): the version is chosen and the key values and evaluated views are evaluated there; `tree` and `outline` are static and the same everywhere (a note says so) |

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

`sources`, `tasks`, `packages` and `cve` are evaluated (`eval::evaluate` of the chosen
file for the default machine and distro). Each ends with a note naming the machine and distro and
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

- `cve` (x4): what OE-core's CVE check reads from the recipe, for the default machine and
  distro, without building anything. **What the metadata declares, not a scan against the
  NVD**; a note says so. Rows `kind cve value file loc`, in this order:
  - `identity`: `CVE_PRODUCT`, `CVE_VERSION`, `CVE_VERSION_SUFFIX` with the operation that
    set them (on Wrynose `spdx-common.bbclass` sets the `??=` defaults `${BPN}` / `${PV}`;
    when nothing sets them the row says the CVE classes default them so);
  - `build` and `unpatched`: when the build directory holds a CVE check JSON report for the
    recipe (tried in order: `CVE_CHECK_RECIPE_FILE_JSON`, `${CVE_CHECK_DIR}` or
    `${DEPLOY_DIR}/cve/<pn>_cve.json`, `<build>/tmp*/deploy/cve/<pn>_cve.json`,
    `${LOG_DIR}/cve/cve-summary.json`, `${DEPLOY_DIR_IMAGE}/<pn>-recipe-sbom.sbom-cve-check.yocto.json`,
    each through the confinement check), its Unpatched / Patched / Ignored counts and the
    version, then one `unpatched` row per Unpatched id with its scores (`v4`, `v3`, `v2`,
    zero scores left out). Without one, a note says how to produce it from the classes the
    layers have (`cve-check`: `INHERIT += "cve-check"`, `bitbake -c cve_check <pn>`;
    `vex`: `bitbake -c generate_vex <pn>`; `sbom-cve-check-recipe`), and that sous-chef
    does not run it;
  - `fact`: inconsistencies worth a person's attention, stated as facts: a CVE both
    patched and in `CVE_STATUS`; a status not in `CVE_CHECK_STATUSMAP` (counted as
    Unpatched by OE-core); a `CVE_STATUS_GROUPS` entry that is not set or has no
    `[status]`; a `cpe:vendor:product` status that matches no `CVE_PRODUCT` word (skipped
    by OE-core) or is malformed; a patch named for a CVE whose header has no `CVE:` line
    or names another id; patches that declare ids only in their header (grouped per id);
    on Kirkstone, a `CVE_STATUS` that has no effect;
  - `status`: every `CVE_STATUS[CVE-...]` entry after `extend_cve_status` (direct flags,
    then `CVE_CHECK_IGNORE` as `ignored`, then each `CVE_STATUS_GROUPS` group's
    `[status]`, each replacing), as `<category> (<detail>): <description>` with `(from
    CVE_CHECK_IGNORE)` / `(from CVE_STATUS_GROUPS <group>)` and the pointer of the
    assignment (its layer is the pointer's); ids in numeric order. A value with python is
    shown as `(depends on python, not evaluated)` and counted in the python note. On an
    OE-core without `CVE_STATUS` (Kirkstone) the rows are `ignore` rows for
    `CVE_CHECK_IGNORE` instead;
  - `patch`: one row per CVE id a patch of the evaluated `SRC_URI` fixes (`by name`, `by
    header`, `by name and header`), with its `Upstream-Status`, its file and the
    `SRC_URI` operation that added it (the layer that added it). A remote, compressed or
    missing patch is matched by file name only (a note lists them).

  Which rules apply (whether `CVE_STATUS` exists, the `cpe:` form, how patch headers are
  read) comes from the workspace's release (since x7, the `release::Feature`s `CveStatus`,
  `CveStatusCpe`, `CvePatchContentLines`) and is named in a note; the `lib/oe/cve_check.py`
  of the first layer that has one is read as a cross-check, and a second note says where it
  differs. Ends with the evaluated views' notes. On the fixture copy of `tests/recipe_cve.rs` (cut):

  ```
  kind	cve	value	file	loc
  identity	CVE_PRODUCT	acme:vuln	-	meta-mine:recipes-sec/vuln/vuln_1.0.bb:9
  identity	CVE_VERSION	1.0 (not set; the CVE classes default it to ${PV})	-	-
  build	-	Unpatched 2, Patched 1, Ignored 1 (version 1.0)	build:tmp/deploy/cve/vuln_cve.json	-
  unpatched	CVE-2024-90001	v3 9.8, v2 6.8	-	-
  fact	CVE-2023-00002	status "maybe-later" is not in CVE_CHECK_STATUSMAP: decode_cve_status warns and counts it as Unpatched	-	meta-mine:recipes-sec/vuln/vuln_1.0.bb:12
  fact	CVE-2024-11111	patched by CVE-2024-11111.patch and also in CVE_STATUS "fixed-version"; CVE_STATUS wins, with a warning	-	meta-mine:recipes-sec/vuln/vuln_1.0.bb:11
  status	CVE-2023-10001	Ignored (not-applicable-platform): Windows only (from CVE_STATUS_GROUPS CVE_STATUS_WIN)	-	meta-mine:recipes-sec/vuln/vuln_1.0.bb:16
  patch	CVE-2025-44444	by name and header; Upstream-Status: Backport [https://example.org/vuln/commit/4]	meta-vendor:recipes-sec/vuln/files/CVE-2025-44444.patch	meta-vendor:recipes-sec/vuln/vuln_%.bbappend:2
  note: what the metadata declares (CVE_PRODUCT, CVE_STATUS, CVE_CHECK_IGNORE, patches in SRC_URI), read as OE-core's CVE check reads it; this is not a scan against the NVD, and nothing was run
  ```

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
note: evaluated for MACHINE=qemux86 DISTRO=testdistro (static replay of the configuration, classes, recipe and bbappends; only allowlisted python helpers evaluated)
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
note: evaluated for MACHINE=qemux86 DISTRO=testdistro (static replay of the configuration, classes, recipe and bbappends; only allowlisted python helpers evaluated)
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
note: key values are evaluated for MACHINE=qemux86 DISTRO=testdistro (static replay of the configuration, classes, recipe and bbappends; only allowlisted python helpers evaluated); use var for the full history
```

### `var`
History and final value of a variable. The main tool.

| Arg | Type | Notes |
|---|---|---|
| `name` | string | base name, e.g. `SRC_URI`; `FILES:${PN}` allowed (expanded for the recipe first); `VAR[flag]` shows that flag only |
| `recipe` | string? | omit for global configuration; `mc:<name>:<recipe>` evaluates in that multiconfig |
| `machine`, `distro` | string? | |
| `mc` | string? | multiconfig, a name in `BBMULTICONFIG` (common section, "Multiconfig"; also `recipe` written `mc:<name>:<recipe>`); not with `authoritative` |
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
value depends on python. (x1) When the expansion met inline python of the evaluated subset
(bitbake-semantics.md, "Inline python evaluated statically"): `note: python evaluated
statically (pure helpers, exact): `bb.utils.contains('DISTRO_FEATURES', ...)`, ...` (two
shown, `+N more`; `; OVERRIDES, which decides its override variants, needed N more` when
`OVERRIDES` did too), and, for the first expression left unevaluated because of what it
reads, `note: python not evaluated: `...`: X holds python that is not evaluated` (the
reason names the variable; `(+N more)`). Python outside the subset gives no such line, only
the python note. `overrides` gives the same lines for `OVERRIDES` (`note: OVERRIDES:
python ...`), `deps` for `DEPENDS` (`build`), `PACKAGES` (`runtime`) and `PACKAGECONFIG`
(`packageconfig`).

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
  real environment; progress notifications while it waits, see the common section). One
  job runs at a time because BitBake allows one client per build
  directory (see `bitbake`): while another job runs, the tool answers from a cached dump if
  the tree is unchanged; otherwise, behind a read-only job (`bitbake op=env`, `op=layers`,
  another authoritative `var`) it waits for that job, up to 30 s, and then runs, and behind
  a build, task, clean or devtool job it returns at once an error naming the running job
  and suggesting static mode. If its own job is still running after 120 s it returns the job id; the next
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

With python (recipe `app`, variable `EXTRA_OEMAKE`): the inline python of `APP_GREETING`
is in the evaluated subset; the anonymous python that appends to the variable is not run,
so the value stays flagged:

```
status	op	override	loc	snippet
shadowed	?=	-	meta-core:conf/bitbake.conf:43	EXTRA_OEMAKE ?= ""
applied	=	-	meta-mine:recipes-app/app/app_0.1.bb:9	EXTRA_OEMAKE = "GREETING=${APP_GREETING}"
final: "GREETING=hello"
note: python evaluated statically (pure helpers, exact): `'hi' if d.getVar('MACHINE') == 'boardx' else 'hello'`
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
| `recipe` | string? | `mc:<name>:<recipe>` allowed |
| `machine`, `distro` | string? | |
| `mc` | string? | multiconfig, a name in `BBMULTICONFIG` (common section, "Multiconfig"; also `recipe` written `mc:<name>:<recipe>`) |
| `show` | `"chain"` \| `"conditional"` \| `"dead"` | default `chain` |

- `chain`: rows `priority entry from loc`: the entries of the evaluated `OVERRIDES`,
  lowest priority first, each with the variables it came through
  (`OVERRIDES <- MACHINEOVERRIDES <- MACHINE`) and the deepest assignment that contributes
  it. Entries are attributed by expanding the applied assignments piece by piece; a piece
  that is python is unknown and said so, unless the python is in the evaluated subset (x1):
  then its text is credited to the assignment (`armv7a` from `arch-armv7a.inc`). `OVERRIDES` is split on `:` with `${...}` kept
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
| `mode` | `"name"` \| `"text"` \| `"regex"` \| `"fts"` | default `name`: statement names containing `query`, case-insensitive, exact names first; `NAME[flag]` (since x7) matches flag statements, the name and the flag each as a fragment, so `PACKAGECONFIG[gnutls]` finds every `PACKAGECONFIG[gnutls] = ...`. `text` (since x7): file content line by line containing `query` literally (case-sensitive, nothing to escape: `PACKAGECONFIG[gnutls] =`). `regex`: the same with a regular expression. `fts`: SQLite FTS5 over names and snippets |
| `kind` | list? | `assign`, `function`, `pydef`, `inherit`, `include`, `addtask`, `deltask`, `addhandler`, `export_functions`, `export`, `unset`, `addfragments`, `addpylib`, `unknown` (every kind the index stores); aliases `task` (`addtask`) and `def` (`pydef`) |
| `layer`, `file_kind`, `recipe` | filters | `layer` takes one name or several separated by commas (since x7: `meta-kiss,meta-kiss-ti`); `recipe` restricts to that recipe's tree (the `.bb`, its includes and classes, its bbappends) |
| `limit` | int | default 30, at most 1000. `budget_tokens` defaults to 1200 here (since x7; 600 elsewhere), room for 30 hits, unless `sous-chef.toml` sets another `[output] budget_tokens` |

Each hit is a row `loc kind item snippet`; `item` is `[name A-B]`, the enclosing item and
its range. `text` and `regex` modes read file content line by line, through the confinement check;
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
  BitBake never reads; another lists the `BBFILES_DYNAMIC` words sous-chef cannot decide
  statically (python, or a variable set later), which are left out.
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
| `recipe` | required; for `providers`, a provided name such as `virtual/kernel`; `mc:<name>:<recipe>` allowed |
| `kind` | `"build"` (default), `"runtime"`, `"packageconfig"`, `"reverse"`, `"providers"`, `"mcdepends"` |
| `mc` | multiconfig, a name in `BBMULTICONFIG` (common section, "Multiconfig"; also `recipe` written `mc:<name>:<recipe>`); provider selection is per configuration, as in BitBake |
| `depth` | default 1, at most 5; used by `build` and `reverse` only (a note says so otherwise) |

There is no `machine` or `distro` argument: the recipe is evaluated for the configuration's
machine and distro (see the common section), in the multiconfig `mc` names when given. `PACKAGECONFIG` additions to `DEPENDS` and `RDEPENDS:${PN}` are made by
OE-core's `base.bbclass` with anonymous python, which is never run, so they are listed from
the flags of the selected options, with a note.

- `build`: rows `depth dep provider source loc`: the words of the evaluated `DEPENDS`, each
  with the assignment that adds it (a class's `DEPENDS:prepend` points into the class) and
  the recipe that provides it (provider selection; `-` when no recipe of the workspace
  does; a `BBCLASSEXTEND` variant such as `-native` is a provider of its name and of its
  rewritten `PROVIDES`, shown as `curl-native (the `native` variant of curl 8.19.0)`), then the build
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
- `mcdepends` (since x5): rows `task from to recipe dep_task status loc`, one per entry of
  the `mcdepends` flag of each task of the recipe (`do_install[mcdepends] =
  "mc:<from>:<to>:<recipe>:<task>"`; bitbake-semantics.md "Multiconfig"): `(default)` for
  an empty configuration name, status `applies here` when `<from>` is the configuration
  evaluated (BitBake adds the dependency only then), `not in this configuration (...)`
  otherwise, `invalid: ...` for an entry without exactly five `:` fields (BitBake stops),
  and a mark when the value holds python; `loc` is the applied assignments to the flag.
  The edges are listed, not followed: the other configuration is not evaluated (call
  `deps` again with `mc=<to>` for that side). `build` says in a note how many there are.
  Tasks added by anonymous python are not seen.
- `providers`: rows `name version status loc`: every recipe that provides the name, the
  selected one first, with the reason for its version and for the choice; then the
  `PREFERRED_PROVIDER` / `PREFERRED_VERSION` assignments that decided it. Candidates are
  recipes whose `PN` or static `PROVIDES` names it, confirmed by evaluating each, and the
  `native` / `nativesdk` variants whose `PN` or rewritten `PROVIDES` names it, and the
  multilib variants (those the configuration's `BBCLASSEXTEND` lists) whose rewritten
  `PROVIDES` names it (a variant whose rewritten `PROVIDES` could not be computed, because
  it depends on python such as a `MULTILIB_VARIANTS` computed by python, is listed as
  uncertain, with a note); other multilib variants by their name only, when nothing else
  provides it. A `PREFERRED_PROVIDERS` entry
  `<name>:<pn>` is used when no `PREFERRED_PROVIDER_<name>` is set (status `selected by
  PREFERRED_PROVIDERS (<name>:<pn>)`, and a `PREFERRED_PROVIDERS` row whose value is the
  provider); a malformed word, conflicting entries and an entry `PREFERRED_PROVIDER_<name>`
  replaces are notes.

```
depth	dep	provider	source	loc
1	libfoo	libfoo 2.0	DEPENDS	meta-mine:recipes-app/app/app_0.1.bb:4
2	autoconf-native	-	DEPENDS	meta-core:classes-recipe/autotoolsish.bbclass:11
2	automake-native	-	DEPENDS	meta-core:classes-recipe/autotoolsish.bbclass:11
2	zlib	-	PACKAGECONFIG[zlib]	meta-core:recipes-core/libfoo/libfoo_2.0.bb:15
note: PACKAGECONFIG rows: OE-Core's base.bbclass adds the build (field 3) and runtime (field 4) dependencies of the selected options with anonymous python, which is not run; they are listed from the flags
note: provider - means no recipe in the workspace provides the name (nor a BBCLASSEXTEND variant of a recipe whose BBCLASSEXTEND lists the class, by its name or through the PROVIDES its class rewrites)
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
| `as_release` | a codename: also check against that release, as if each layer claimed it in `LAYERSERIES_COMPAT` (`--as-release`); an unknown name is an error listing the supported ones, one older than Kirkstone is refused |
| `only_release` | with `as_release`: check against that release alone |
| `migrate_to` | a codename: the migration report instead of the findings (`--migrate-to`; lint-rules.md, "Running lint"): findings new with that release, findings that already hold, the features it changed that the layers use with their pointers, each layer's `LAYERSERIES_COMPAT` and what to write, what the report cannot tell. Text is cut to the budget by whole lines (a last line says so); `format=json` returns the whole `sous-chef-migration/1` object. Not with `as_release` |

The tool has no `all` argument (the CLI's `--all`) and no `mc`: recipe-scope rules evaluate
each recipe in the default configuration only, and the files it skips as not collected are
those of the default configuration's `BBMASK` (bitbake-semantics.md, "Multiconfig"). A
pointer with a line range keeps only
the findings in that range. Output: a summary line first (counts by severity and by rule,
so a table cut by the budget still gives the totals), then the table
`severity rule pointer message`, then the notes, the last of which (since x7) says what was
checked: `checked N files with R rules: Yocto conventions and release compatibility, not
whether the recipes build or work`, plus the recipes recipe-scope rules missed because their
evaluation failed (layers left out have their own `not checked:` note), so that few or no
findings are not read as a clean bill of health. JSON:
`{summary, errors, warnings, infos, by_rule, findings}`, where `findings` is the table.
See [lint-rules.md](lint-rules.md) for the engine.

```
4 findings in 6 files: 3 error, 1 warning, 0 info; by rule: hygiene/parse-error 4
severity	rule	pointer	message
error	hygiene/parse-error	meta-mine:conf/distro/mine.conf:1	`inherit` in a .conf file: BitBake's configuration parser rejects it as an unparsed line; move it to a class or recipe (use INHERIT += for a global class)
error	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:3	unparsed line; fix the line so BitBake can parse it, or remove it
warning	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:4	addtask: `before`/`after` groups split by the other keyword are concatenated by BitBake without a space, joining two task names; write all `after` tasks in one group and all `before` tasks in another
error	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:5-6	function do_broken is never closed: no `}` in column 0 before end of file; close the function with `}` in column 0
note: checked 6 files with 1 rule: Yocto conventions and release compatibility, not whether the recipes build or work
```

Fix mode (lint-rules.md, "Fixes"; the CLI's `--fix`, `--fix-suggested`, `--write`):

| Arg | Notes |
|---|---|
| `fix` | `true`: fix what the rules can fix safely; a dry run unless `write` |
| `fix_suggested` | `true`: also suggested fixes (they change meaning as the rule recommends); implies `fix` |
| `write` | `true` with `fix`: write the fixes into the writable layers; `write` without `fix` is an error |

`target`, `rules` and `severity` select the findings as above. Text: a `lint --fix` line
(saying whether it is a dry run), the lint summary line, `fixes available: N for M findings
(S safe, G suggested); by rule: ...`, then per round `applied N fixes in F files` (dry run:
`would apply`) followed by a unified diff per file (`--- layer:path` / `+++ layer:path`),
the fixes deferred because they overlap another (dry run: "re-run after applying"; since x7
a dry run also says that it shows the first round only and a write may apply further fixes
in up to 5 rounds), the
files refused (changed since indexed or read, refused by the scaffold checks), the
suggested fixes not applied (each `pointer rule safety description` with its own diff), the
fixes in layers that are not writable (never applied), and after a write `after: F files
written; N findings fixed, R remaining, X new, V revealed in them` with the new and revealed
findings listed. The text is cut at the budget on a line boundary, with a note. JSON: the
whole report, `{write, suggested, report, available, rounds: [{changes: [{file, diff,
fixes}], deferred, refused}], suggested_skipped: [{fixed, diff}], not_writable, after:
{files, fixed, remaining, new, revealed, problems}}`, each fix as `{finding, fix: {layer,
path, sha256, edits: [{bytes: {start, end}, replacement}], description, safety}}`.

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

The plan is refused (no files; the notes say why) when the move would change what BitBake
parses. Every version is evaluated as it is and with the planned files in memory (nothing
is written): any difference in a final value, a flag, a function's pieces, the anonymous
python or the `addtask` / `deltask` / `addhandler` order refuses the split, and the notes
name each variable with both values (`greet_1.0.bb: GREET_FLAGS would change from "--old
--shared" to "--old"`). A static check runs first and finds the possible problems: a
statement that stays in a `.bb` would come after a moved one it does not commute with (they
set a common variable or flag, or one expands immediately, with `:=` or in `inherit` /
`include`, what the other sets, the classes and files an `inherit` / `require` pulls in
counted as part of it), or a moved statement expands `FILE`, `PN`, `PV`, `BPN`, `BP`, `P`
or `PF` immediately (inside the `.inc`, `FILE` is the `.inc`). Those that involve python
(which the evaluator does not run, so it cannot see the change) or a file that cannot be
read refuse the split on their own; the others are listed after the evaluated changes when
there are some, and otherwise do not stop the split (a note says the static check could not
prove it safe and the evaluation shows no change). Also refused: versions in different
directories, an existing `<pn>.inc`, a version that does not parse cleanly, versions with
nothing in common.

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
note: greet_1.1.bb: GREET_FLAGS would change from "--new --shared" to "--new"
note: greet_1.0.bb: GREET_FLAGS would change from "--old --shared" to "--old"
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
| `target` | recipe or image; for `build`, `task`, `env` and `clean` also `mc:<multiconfig>:<target>` |
| `task` | for `op: task`, e.g. `compile` (a `do_` prefix is dropped) |
| `args` | op-specific, validated against an allowlist (below) |
| `confirm` | required true for destructive operations |
| `job` | for `status` and `cancel`: `job:<n>` or `<n>`; default: the running job, else the last one |
| `wait` | seconds to wait for the job to end before answering (0 to 300, default 0); for a starting operation and `status` |

| `op` | Runs | `args` | Handle kind |
|---|---|---|---|
| `build` | `bitbake [-k] <target>` | `continue` (`-k`) | `build` |
| `task` | `bitbake -c <task> [-f] <target>` | `force` (`-f`) | `build` |
| `env` | `bitbake -e [<target>]` | none | `env` |
| `clean` | `bitbake -c <which> <target>` | one of `clean` (default), `cleansstate`, `cleanall` | `build` |
| `devtool` | `devtool <subcommand> ...` | the subcommand first: `modify`, `status`, `finish`, `reset`, `build` | `build` |
| `layers` | `bitbake-layers <subcommand> [<target>]` | one of `show-layers` (default), `show-appends`, `show-recipes`, `show-overlayed`; `target` only with `show-recipes` and `show-appends` | `text` |

A target is validated before any argv is built (`validate_name`: letters, digits and `_ . +
- /`, not starting with `-`). Since x5 `build`, `task`, `env` and `clean` also take exactly
`mc:<multiconfig>:<target>` (`bitbake::env::validate_target`: the name and the target each
pass `validate_name`); every other form with a `:` is still rejected (`mc::<target>`,
`multiconfig:...`, `mc:*:...`, `<target>:do_x`, more colons), and `devtool` and `layers`
take plain names only. The multiconfig name is not checked against `BBMULTICONFIG` there:
BitBake reports an unknown one.

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
in kas's work directory, so kas does not rewrite `conf/` first; not used when a kas file
has a `{repo, file}` include, because `-k` still fetches and checks out those repositories:
the modes below are tried instead); a build directory holding
bitbake-setup's `init-build-env` (`bitbake-setup`); an `oe-init-build-env` in a layer root,
a layer root's parent or the build directory's parent (`init-script`). The init script is
sourced and the program run by a wrapper that takes every name as argv; in `kas` mode the
command string is made only of validated words, each single-quoted.

Every operation except `status` and `cancel` starts a background job and returns at once
with `job:<n>`. One job of any kind runs at a time, because BitBake allows one client per
build directory: a second client finds the lock taken, waits and retries, and fails with
"Unable to connect to bitbake server, or start one" after about 20 s (BitBake
`lib/bb/main.py`; live, a `bitbake-layers` started during a 4 s `bitbake -e` waited and
succeeded, live-verification.md "Two clients"). So sous-chef never starts a second BitBake
client, and what happens to a request depends on the running job:

- while a read-only job runs (`env`, `layers`, or the `bitbake -e` of an authoritative
  `var`; seconds to a minute), the request waits for it, up to 30 s, and then starts;
- while a `build`, `task`, `clean` or `devtool` job runs (minutes to hours), the request
  starts nothing and returns at once the running job, with "already running (one job at a
  time)" and why; call again once `op=status` shows it ended, or `op=cancel` it;
- a read-only job still running after the 30 s gets the same answer.

`wait` (seconds, 0 to 300; default 0) makes a starting operation, or `op: status`, wait
for the job to end before answering; the answer is then the finished job (its handle, its
tail or digest). While it waits, and while a request waits behind a read-only job, a client
that sent a progress token gets progress notifications (common section): the seconds since
the call began, and the job with its last output line (`job:1 build hello running 12s:
NOTE: Running task 812 of 3301 ...`). Without `wait`, poll with `op: status`.

`status` reports `job`, `op`, `target`, `command`, `state` (`running`, `succeeded`,
`failed`, `cancelled`), `elapsed`, `exit` and `handle`. While running it shows the last 5
output lines; a finished job that did not fail shows the last 3 and a `slice` hint; a
failed job is followed by a digest of the console output and, for operations that run
tasks, a `log job=` hint. JSON: `{job, tail, digest, notes}`. `cancel` terminates the job's
process group (SIGTERM, then SIGKILL after 2 s, then up to 5 s until no process is left in
the group; a process that left the group, such as a daemon that called `setsid`, is not
reached) and reports the job, which is `cancelled` once that is done; a `bitbake -e` that
times out is ended the same way. A finished job's output becomes a handle the next time the
`bitbake` tool, or `log` with `job`, is called.

Jobs live in the server process. `status` of a job, finished or not, is reachable only
while that server runs; a new server starts with no jobs. When the server exits because the
client closed the connection, a running job's process group is sent SIGKILL (nothing waits
for it; checked by `tests/bitbake_jobs.rs`,
`a_running_job_is_killed_when_the_server_runtime_is_dropped`) and its output is not stored.
A server killed by a signal kills nothing: the job's group is a group of its own and keeps
running. The output being written lies in `<cache dir>/jobs/<pid>-<n>.out`; the next server
removes such files when it starts (on a thread of its own, so start-up does not wait) when
the pid is not a live process and the file was last written more than 24 hours ago (a file
of a live pid is never removed; checked by `tests/mcp_server.rs`,
`server_start_removes_output_files_of_a_dead_server`).

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

A handle lasts until retention removes it, which happens when the server starts: handles
last stored more than `[handles] max_age_days` ago (default 14) are deleted, then the
oldest until the store holds at most `[handles] max_total_mb` (default 500 MB); a handle
stored by the running server is never deleted. An expired handle is a `NotFound` error that
says to rerun the command. `status` shows the store's size and the limits.

### `status`
No arguments. Workspace roots, the release (`release`, e.g. `wrynose (LAYERSERIES_CORENAMES)`),
layer count, file counts by kind, index path and age, whether a build directory and a usable
bitbake environment were found, and the running job if any (`job`, e.g.
`job:1 task hello running 1s`), and the handle store (`handles`: how many handles it holds,
their total size, and the retention limits it applied at start, `[handles] max_total_mb` and
`max_age_days` in `sous-chef.toml`, e.g. `12 stored, 3.4 MB; limits 500.0 MB, 14 days`; JSON
`handles {count, bytes, max_total_bytes, max_age_days, error}`). Layers are named by their directory (`meta`,
`meta-arm-toolchain`) in every tool and pointer; BitBake 2.18's `bitbake-layers
show-layers` prints the collection name instead (`core`, `arm-toolchain`).

When the workspace has multiconfigs, a multiconfig file or a problem with them (since x5;
the default configuration is evaluated for this only when the index has a `BBMULTICONFIG`
statement or a `conf/multiconfig/*.conf` file, or the kas targets name a multiconfig), it
adds `multiconfigs` (where the names come from: `BBMULTICONFIG = "tiny musl"
(build:conf/local.conf:21)`, or the kas targets in a kas project without a build
directory; `; may be wrong: ...` when the value depends on python or an unevaluated kas
header), one `multiconfig` row per name with its file (`musl
build:conf/multiconfig/musl.conf:1`, or `ghost (no file)`), one `multiconfig_present` row
per `conf/multiconfig/*.conf` along `BBPATH` that no name uses (`(not in BBMULTICONFIG)`,
or `(present, not confirmed enabled)` when the value is uncertain) and one
`multiconfig_problem` row per thing BitBake would stop on (a listed name without a file, a
name starting with a digit); JSON `multiconfig {source, uncertain, enabled, present,
problems, error}`. A workspace with `BBMULTICONFIG ?= ""` only (what kas writes) shows none
of it.

In a kas project (and only then) it adds `kas` (the files as kas takes them and how they
were found, e.g. `kas/kiss.yaml:kas/dogbonedark.yaml (from kas in sous-chef.toml)`),
`kas_defaults` (`machine dogbonedark, distro kiss, target kiss-image`: what kas uses, so a
`KAS_MACHINE`, `KAS_DISTRO` or `KAS_TARGET` in the environment sous-chef was started in
wins over the files, as in kas), one `kas_env` row per setting such a variable overrides
(`KAS_MACHINE=board2 overrides machine "board1" (kas/board.yaml:5)`; `KAS_TASK` too),
`kas_layers` (how
many layers the kas files name, and those not checked out yet), `kas_lock` (lock files
read), `kas_mismatch` (layers in the kas files but not in `bblayers.conf`, and the reverse:
the kas files changed since the last checkout) and one `kas_problem` row per problem; in
JSON, a `kas` object with the same fields (`env_overrides` only when there is one). The `bitbake` row's mode is `init-script`,
`bitbake-setup`, `kas` or `initialised`.

### `audit`
A findings report over a whole build repository (T8.6, `src/audit.rs`): every layer under
the root, enabled or not, with its releases, structure, lint findings and parse problems,
for a person to read. It adds no analysis of its own; it runs layer discovery, release
detection, `org/layer-conf`'s checks, the `BBLAYERS` order checks of `layers
show=compat` (`layers::order_problems`), `layers show=appends`,
`query::shadowed`, `query::orphan_appends`, every enabled lint rule as `sous-chef lint
--all`, and the index's parse and resolution results, and assembles them. It is
read-only: it writes nothing into the repository, runs no BitBake and no kas, fetches
nothing; it refreshes the index in the cache directory as every tool does. Like lint, it is
for the default configuration only (its lint findings, the files not collected); the
multiconfigs are not listed in it (`status` lists them). The lint run
dominates its time: on the 1755-file live workspace a run takes 15 to 30 s (lint alone
about 15 s; the rest depends on the machine's load).

| Arg | Notes |
|---|---|
| `root` | the repository root, relative to the working directory: the working directory (default, when it holds the build directory or an enabled layer; else the workspace span below), or a directory inside the workspace span, the deepest directory holding the build directory, every enabled layer and the kas project root. The filesystem root and anything outside are refused, so a client cannot point the audit at other directories. |
| `budget_tokens`, `format` | for the digest only; the stored report is never cut |
| `migrate_to` | a codename: add section 7, the migration report of the writable layers to that release (as `sous-chef lint --migrate-to`), and a `7 migration` digest row; the JSON report gets a `migration` field. Without it the report is unchanged |

The tool stores the whole Markdown report under a `text` handle and returns a digest:
rows `field value`: `summary`, `workspace`, one row per section (`1 inventory` to `6 not
checked`) with that section's summary line, then `attention N` per item most worth
attention; then the handle with the report's size and the `slice` hint. Rows are dropped
from the end to fit the budget (attention items first); the hint always stays. `slice
handle=... page=N` pages through the report (`tests/audit.rs` reassembles the report from
the pages). The digest of the fixture workspace (`tests/snapshots/audit__audit_tool_digest.snap`,
handle masked):

```
field	value
summary	workspace, release wrynose: 3 layers enabled (1 own), 1 present but not enabled; 21 indexed files; findings in own layers: 0 error, 3 warning, 1 info; in upstream layers: 1 error, 1 warning, 2 info; 3 items worth attention
workspace	root .; release wrynose (LAYERSERIES_CORENAMES); build build
1 inventory	3 layers enabled (1 own, 2 upstream), 1 present but not enabled; 0 repositories
2 releases	workspace release wrynose (LAYERSERIES_CORENAMES); every enabled layer claims it; 1 layer present but not enabled does not
3 structure	layer.conf and BBLAYERS order: 0 (0 error, 0 warning, 0 info); shadowed recipe or class names: 0; bbappends matching no recipe: 1 (1 in own layers); bbappends not collected: 0; over-broad bbappends and duplicate versions: 1
4 lint	8 findings in 21 files: own layers and build directory 4 (0 error, 3 warning, 1 info); upstream layers 4 (1 error, 1 warning, 2 info)
5 parse	0 unparsed statements, 4 unresolved include/require, 0 unresolved inherits, 0 indexed files BitBake does not collect
6 not checked	build environment not found; anonymous python, which is not run, in 1 recipe and 0 classes; layers present but not enabled, not analysed: 1
attention 1	warning hygiene/release-syntax x1 (meta-mine): meta-mine:recipes-app/app/app_0.1.bb:22 `${WORKDIR}/app.c`: from styhead on, ...
...
report h:XXXXXXXXXX: 171 lines, 10433 bytes of Markdown; attention items are chosen by: errors in own layers, then layer structure errors, then warnings in own layers
slice handle=h:XXXXXXXXXX page=1 | grep=RE | lines=A-B
```

The CLI writes the report itself: `sous-chef audit [ROOT] [--format md|json] [--output
FILE]` (ROOT default: the working directory; discovery starts there, and the global
`--build-dir`, `--config` and `--layer` are taken relative to the working directory as
for every command). Progress goes to stderr, never into the report; the `audit` tool sends
it as MCP progress notifications when the request carries a progress token. Exit status 0 when the
report was produced, whatever it finds (`sous-chef lint --fail-on` is the command that
fails on findings); 2 when it could not be produced.

**What it covers.**

- Layers: the workspace's (`bblayers.conf`, a kas project's, or those found on disk), plus
  every directory with `conf/layer.conf` at most 5 levels below the root, also below each
  submodule path a `.gitmodules` declares (`git config -f .gitmodules --get-regexp`) and
  each kas repository. The walk does not follow symlinks and never enters hidden
  directories, build directories (`conf/bblayers.conf` or kas's `CACHEDIR.TAG`), `tmp*`,
  `sstate-cache`, `downloads`, or `lib`, `scripts`, `test`, `tests`, `testdata` (BitBake's
  own sample layers live there). A layer present but not enabled is listed from its
  `conf/layer.conf` alone (collection, priority, `LAYERSERIES_COMPAT`, `LAYERDEPENDS`,
  read only inside the layer); it is not in the index, so it is not counted, linted or
  resolved, and the report says so. A `LAYERDEPENDS` on a collection no layer under the
  root adds is noted.
- Git: per repository holding a layer (or named by the kas files) inside the root:
  remote (`origin`, else the first; any `user:password@` removed), revision, branch,
  `git status --porcelain` entries, the submodule that declares it. Read-only commands
  only (`rev-parse`, `remote`, `status`, `config -f .gitmodules`), argv only, a 5 s
  timeout, no optional locks, no fsmonitor, the `GIT_DIR` family removed. Any failure is
  "unknown", never an error. A repository enclosing the root is not reported.
- kas: each repo's pinned commit and where it comes from (the lock file that sets
  `overrides: repos: <id>: commit:`, else the kas file), whether the checkout is at it
  (**not at pin** otherwise), the lock files read and the kas files without one. When the
  kas files are not known (several candidates, no `kas = [...]`), the report says so and
  shows no pins.

**The Markdown report.** One `Generated:` line (the time and the version: the only line
that differs between two runs on the same repository state), then:

- `## Summary`: one line; a table (root, build directory, kas files, release and where
  it came from, layer counts, file counts, findings by severity for own and for upstream
  layers); `### Most worth attention`, 3 to 10 items chosen by a stated rule: errors in
  own layers by rule (most findings first); layer structure errors outside own layers
  (`org/layer-conf` errors; bbappends that match no recipe); warnings in own layers by
  rule; and while that gives fewer than 3, errors in upstream layers, layer structure
  warnings (`BBLAYERS` order), infos in own layers.
- `## 1. Inventory`: `### Layers` (BBLAYERS position, name, path, role `own` /
  `upstream` / `not enabled`, collection, priority, `LAYERSERIES_COMPAT`, `LAYERDEPENDS`,
  indexed file counts), `### Repositories` (with the kas repo and pin columns in a kas
  project), `### kas project`, notes.
- `## 2. Releases`: the workspace release and its source, the supported releases and the
  floor, and a matrix of layer against claimed release (columns: the supported releases
  some layer claims plus the workspace's; `**missing**` where a layer does not claim the
  workspace release; names older than the floor or unknown in the last column).
- `## 3. Layer structure`: `org/layer-conf`'s problems (through `layer_problems`, so they
  agree with the rule) and the `BBLAYERS` order rows of `layers show=compat` (through
  `layers::order_problems`, the function the view calls; their shared
  explanation said once); shadowed recipes and classes; bbappends that match no recipe and
  those BitBake does not collect (`layers show=appends`); `org/append-wildcard-broad` and
  `org/duplicate-versions` findings from the lint run.
- `## 4. Lint findings`: own layers and the build directory first, every finding grouped
  by layer then rule; then upstream layers: a table of counts per layer with the most
  frequent rules, and per rule its count by severity and the first 3 findings (worst
  severity first) followed by the command that lists all of them (`sous-chef lint --all
  --rule <id>`). Release-dependent findings name the release in their message.
- `## 5. Parse and resolution problems`: unparsed statements, unresolved `include` /
  `require` and `inherit` (whether the argument needs evaluation, or what BitBake does with
  a missing file), files
  BitBake does not collect and why. Own layers in full, at most 5 per upstream layer, what
  BitBake stops on first.
- `## 6. What was not checked`: whether a build environment was found, files with
  anonymous python by kind, the layers not analysed, and the limits of static analysis.
  How many values depend on python is not counted: that needs evaluating every recipe.

Every section starts with a one-line summary. Pointers are `layer:path:line`; every other
path is relative to the root, and the root's absolute path is removed from messages. The
fixture workspace's report is `tests/snapshots/audit__audit_fixture_md.snap`.

**JSON** (`--format json`): the same content. Top-level fields: `schema`
(`"sous-chef-audit/1"`, bumped when a field is renamed or removed), `generated`,
`sous_chef_version`, and one object per section, each with a `summary` string:

| Field | Content |
|---|---|
| `summary` | `line`, `root` (`.`), `build_dir`, `kas_files`, `release`, `release_source`, `release_note`, `counts` (`layers_enabled`, `layers_not_enabled`, `layers_own`, `layers_upstream`, `files` by kind, `files_total`, `findings_own` and `findings_upstream` as `{error, warning, info}`), `attention_rule`, `attention` (`section`, `severity`, `what`, `count`, `layers`, `pointer`, `message`) |
| `inventory` | `layers` (`name`, `path`, `enabled`, `position`, `role`, `collections`, `priority`, `compat`, `depends`, `repository`, `files` `{bb, bbappend, inc, bbclass, conf}` or null, `note`), `repositories` (`path`, `remote`, `revision`, `branch`, `dirty`, `submodule`, `kas_repo`, `pin`, `pin_from`, `at_pin`, `kas_own`, `checked_out`, `layers`), `kas` (`files`, `source`, `lock_files`, `without_lock`, `not_in_bblayers`, `not_in_kas`, `problems`) or null, `notes` |
| `releases` | `workspace`, `source`, `note`, `supported`, `floor`, `columns`, `layers` (`layer`, `enabled`, `claims`, `older_than_floor`, `unknown`, `claims_workspace`) |
| `structure` | `problems` and `order` (`layer`, `check`, `severity`, `pointer`, `message`), `shadowed` (`kind`, `name`, `wins`, `shadowed`), `orphan_appends`, `appends_not_collected` (`append`, `status`), `lint` (findings: `severity`, `rule`, `pointer`, `message`), `notes` |
| `lint` | `command`, `files`, `own_total`, `upstream_total`, `own` and `upstream` (per layer: `layer`, `counts`, `rules`: `rule`, `severity` (the worst), `count`, `counts`, `findings`, `more`, `command`), `notes` |
| `parse` | `unparsed`, `unresolved_includes`, `unresolved_inherits` (each `{total, by_layer: [{layer, count}], items: [{pointer, text}], more}`), `not_collected` (`reason`, `files` as the same listing) |
| `not_checked` | `build_environment`, `anonymous_python` (files by kind), `layers_not_analysed`, `limits` |

Unknown values are `null` (a revision git could not give, a layer that is not indexed).
The fixture's JSON is `tests/snapshots/audit__audit_fixture_json.snap`.

**On the live workspace** (live-environment.md, the kas build, `--config` a copy of
`scratch/sous-chef.toml` with the two kas files added): 16 to 31 s, 39.3 KB of Markdown
(385 lines), 76 KB of JSON on 2026-11-17; re-run for branch `w9-final` (2026-11-18, release
binary): 15.4 s, 38.5 KB of Markdown (379 lines). The report holds no absolute path; the
excerpt below, from the w9-final run, is cut, not edited:

```
## Summary

kas project, release wrynose: 5 layers enabled (2 own), 6 present but not enabled; 1755 indexed files; findings in own layers: 0 error, 0 warning, 1 info; in upstream layers: 0 error, 166 warning, 458 info; 2 items worth attention
...
1. **warning** `BBLAYERS order` (1 finding, in meta-arm): `build:conf/bblayers.conf:1-6` meta-arm comes before meta in BBLAYERS, though LAYERDEPENDS_meta-arm names `core` (added by meta); BBPATH lists meta-arm first, so its conf/machine/include/arm/arch-armv8-7a.inc, conf/machine/include/arm/arch-armv8-8a.inc, conf/machine/include/arm/arch-armv9-1a.inc, conf/machine/include/arm/arch-armv9-2a.inc, conf/machine/include/arm/arch-armv9-3a.inc are found instead of meta's by include/require, inherit and conf lookups; list meta first if that is not intended
2. **info** `hygiene/missing-metadata` (1 finding, in meta-kiss): `meta-kiss:recipes-kernel/linux/linux-kiss_7.0.bb:1` recipe has no HOMEPAGE; set HOMEPAGE to the upstream project's web page

## 1. Inventory

5 layers enabled (2 own, 3 upstream), 6 present but not enabled; 4 repositories (1 with uncommitted changes)
...
| - | meta-kiss-amd | meta-kiss-amd | not enabled | meta-kiss-amd | 9 | wrynose | core meta-arm meta-kiss meta-python xilinx-microblaze | not indexed |
...
| repository | remote | revision | status | submodule | kas repo | kas pin | layers |
|---|---|---|---|---|---|---|---|
| . | https://github.com/bootlin/yocto-kiss | cc621c5d5725 (wrynose) | dirty (1) | - | meta-kiss, meta-kiss-ti | own repository (kas does not check it out) | meta-kiss meta-kiss-amd meta-kiss-nxp meta-kiss-st meta-kiss-ti |
| bitbake | https://git.openembedded.org/bitbake | 6e260eb6ab16 (2.18) | clean | - | bitbake | 6e260eb6ab16 (kas/kiss.lock.yaml): at pin | - |
| meta-arm | https://git.yoctoproject.org/meta-arm | 673e8d7c5dcd (wrynose) | clean | - | meta-arm | 673e8d7c5dcd (kas/kiss.lock.yaml): at pin | meta-arm meta-arm-bsp meta-arm-toolchain |
| openembedded-core | https://git.openembedded.org/openembedded-core | 33a6bd6cc8a9 (wrynose) | clean | - | openembedded-core | 33a6bd6cc8a9 (kas/kiss.lock.yaml): at pin | meta meta-selftest meta-skeleton |
...
- Note: meta-kiss-amd (not enabled) has LAYERDEPENDS on meta-python, xilinx-microblaze, which no layer under the root adds: it cannot be enabled until a layer providing them is added
```

Read as its reader would, the first run (2026-11-17) was right about the repository but
carried two errors of the rules it calls, both fixed since: its first attention item was 3
`hygiene/parse-error` errors in `meta:conf/ccache.conf`, ccache's own configuration file,
which lint then took for metadata (a data file since w6), and section 5 listed `inherit
sanity` in `meta:classes/sign_*.bbclass` as not found, because the index resolved an
inherit in a `classes/` class as a recipe inherit while those classes are used through
`INHERIT`, where `classes-global/sanity.bbclass` is found (w7: resolved in both contexts;
unresolved inherits 24 -> 21, still 21 in the w9-final run: 20 in `meta`, 1 in
`meta-arm`, each an argument that needs evaluation). The `dirty (1)` of yocto-kiss is the
lock file `live-env.sh` wrote. With `scratch/sous-chef.toml` unchanged (no `kas` key) kas
detection finds five candidate kas files, so the report shows no pins and says why.

### `diff`
A statement-level diff of a layer's recipes between two git revisions, and what it means
for the bbappends of the workspace's own (writable) layers (T8.4, `src/diff.rs`). The
question it answers: an upstream layer moved (a new release branch, a stable update);
which of the recipes we use changed, and which of our bbappends no longer apply, now apply
to another version, or carry patches whose source changed.

| Arg | Notes |
|---|---|
| `layer` | required: a layer name of the workspace; the git repository is the one holding the layer's directory (a layer outside any repository is an error) |
| `from` | required: the old revision, a branch, tag or commit of that repository |
| `to` | the new revision; default **the working tree** (`HEAD` plus uncommitted and untracked files): that is what the workspace builds now and what `item` reads, so the `to` pointers can be opened directly. Give `HEAD` to compare committed states only |
| `recipe` | one recipe by `PN`; an unknown name is an error listing similar names of the layer |
| `all` | every recipe of the layer (unchanged ones are counted, not listed) |
| `budget_tokens`, `format` | as every tool; a diff over the budget is stored whole under a `text` handle, in the format asked for: the tab-separated table for `text`, the `sous-chef-diff/1` document (the CLI's JSON, pretty-printed one field per line, so `slice` pages and greps it) for `json` |

Default selection (no `recipe`, no `all`): the recipes of the layer that the writable
layers (other than `layer`) **append** (a bbappend whose `PN` is the recipe's) or **use**:
a static word of `DEPENDS`, `RDEPENDS*`, `RRECOMMENDS*`, `IMAGE_INSTALL`,
`CORE_IMAGE_EXTRA_INSTALL`, `PACKAGE_INSTALL`, `TOOLCHAIN_*_TASK` or
`PREFERRED_PROVIDER_*` that is the recipe's `PN` (also after removing `-native`, `-cross`,
`-crosssdk`, `nativesdk-`), or a `PREFERRED_VERSION_<pn>`; plus, per own file that
`require`s / `include`s files of the layer (the index's edges), those files as one tree.

**Revisions.** Validated before they reach git: no leading `-`, no whitespace or control
character, no `:` (a `rev:path`), no `..` (a range), at most 200 characters; then resolved
with `git rev-parse --verify --end-of-options <rev>^{commit}`, and only the full hash is
used after that. A revision that is not a commit is an error; nothing is fetched. Git is
run read-only (`ls-tree`, `cat-file --batch`, `ls-files`, `diff --name-status`,
`rev-parse`), argv only, with a timeout, no optional locks, no fsmonitor and the
`GIT_DIR` family removed; nothing is checked out and nothing is written into the
repository. The old side is read with one `git cat-file --batch` of every `.bb`, `.inc`,
`.bbclass` and `.bbappend` of the layer; the working tree from disk (a symlink out of the
layer is not read).

**What is compared.**

- *Recipe identity*: the `PN` of the file name, as the scanner derives it; the `.bb`
  files at a revision are those **that revision's** `BBFILES` globs match: the words its
  `conf/layer.conf` adds to `BBFILES` (`+=`, `=+`, `.=` / `=.` with a space, `:append`,
  `:prepend`; `=` and `:=` replace them), with `${LAYERDIR}` replaced by the layer's
  directory, read with git from the revision. When that file is the one on disk the
  workspace's current `BBFILES` is used; when the text cannot decide it (`?=` / `??=`, an
  `include` / `require`, `:remove` or another override, `unset BBFILES`, a reference to a
  variable other than `LAYERDIR`, no `conf/layer.conf`), the current `BBFILES` is used and
  a note says so. The workspace's active `BBFILES_DYNAMIC` globs for the layer are added at
  both revisions; `BBMASK` is not applied. Several versions of one `PN`: equal versions
  pair first; each remaining old version, highest first, pairs with the new version that
  shares the longest version prefix (`fiptool-native` 2.10.4 with 2.10.30, not 2.14.1);
  a pair with different versions is a version change (`renamed`), the rest are added or
  removed. A `.bb` in another directory is `moved`.
- *The tree*: the `.bb` and the files it `require`s / `include`s **inside the layer at that
  revision** (relative to the including file, then to the layer root, as BitBake's search
  starts; `${PN}`, `${BPN}`, `${PV}`, `${BP}`, `${THISDIR}` from the file name). An include
  found in another layer, an absolute path outside the layer, a missing file or an argument
  needing evaluation is a `file ... not compared` row saying which. Classes are not part of
  a tree.
- *Statements, as written, never evaluated*: assignments keyed by the left-hand side as
  written (`SRC_URI:append:qemux86 +=`, `SRC_URI[sha256sum] =`), so the operations of one
  variable and its override and flag variants sort together. A statement in the legacy
  `_` override syntax (`DEPENDS_append +=`) is keyed with its `:` form, so it pairs with
  the same statement in the `:` syntax, but the two never match: a rewrite from one to the
  other is `changed`, with `rewritten from the legacy _ override syntax: DEPENDS_append +=
  -> DEPENDS:append +=` (and the value change, if any); the `name` column shows the key as
  written at `to`. Functions and tasks by name
  (bodies compared; the first differing code line shown); `inherit` per class;
  `require`, `include`, `addhandler`, `EXPORT_FUNCTIONS` and unparsed lines as
  directives; and `SRC_URI` word by word (`parse::words`, so a `${@...}` expression is one
  entry). A statement that moved between files of the tree is `changed` only if its text
  changed. Statements that differ only in whitespace, and tree files that differ only in
  comments, blank lines or line positions, are counted, not listed. More than 20 flag
  statements of one variable with one change (a kernel's `CVE_STATUS[...]`) are one row in
  the table; the CLI's JSON lists each.
- *Classes*: every `.bbclass` of the layer that differs, once, with the statement count,
  the compared recipes that inherit it directly, and the own files that inherit it,
  directly or through other files (`query::inheritors` on the index as the workspace is
  now); a `classes-global/` class is marked as able to affect every recipe.

**Your bbappends.** Every bbappend of a writable layer whose `PN` is a compared recipe's,
matched by file name exactly as `resolve::append_matches` (the `%` rule), at each revision:

| Severity | When |
|---|---|
| `no longer applies` | it matched at `from`, not at `to`: names the file and version it applied to and the recipe now there, says it is pinned when it has no `%` and suggests the rename; BitBake stops on a dangling bbappend (a warning only with `BB_DANGLINGAPPENDS_WARNONLY`, before Walnascar) |
| `applies to a different version` | a `%` append matched both, the version changed |
| `source changed under a patch` | it carries `SRC_URI` patches and the recipe's source changed: version (file name), `PV`, `SRCREV*`, `LINUX_VERSION`, `KBRANCH`, `SRCBRANCH`, `BRANCH`, a fetch URL (a non-`file://` entry) or a `SRC_URI[...sum]` checksum |
| `recipe changed` | still applies, the recipe tree changed |
| `now applies` | it did not match at `from` |
| `unchanged` | still applies to an unchanged tree |

Each append row adds the variables and functions it sets that changed in the recipe (`it
sets SRC_URI, do_install, which changed in the recipe`) and the source changes; one row
per patch gives the patch file found next to the append (`<dir>/${PN}`, `${BPN}`, `${BP}`,
`files`, `<dir>`), "the source it applies to changed: check it" when so, and whether a
patch of the same name was **added** to the recipe upstream (in its `SRC_URI` or under its
directory: the fix may have been upstreamed) or **removed** from it (the fix may now be in
the source). Whether a patch still applies is never claimed: that needs the source, which
is not fetched.

**Output.** Rows `recipe kind name change value from to`; `from` and `to` are pointers at
each revision (`from` pointers name files at the old commit: read them with `git show
<hash>:<path>`, the hash is in the notes). First `summary` (recipes compared, changed,
renamed or moved, added, removed, unchanged; own files' includes; bbappends affected by
severity; classes changed), `revisions` and `layer files` (files of the layer that differ,
by kind, from `git diff --name-status` plus untracked files); then the bbappends, most
severe first, each followed by its `bbappend patch` rows; then per recipe (own-appended
first, then by name, then the own files' includes) a `recipe` row (version, number of
changes, why it was selected, source changes) and its `file`, `var`, `function`, `task`,
`inherit`, `directive` and `SRC_URI entry` rows; then `class` rows; notes last. Everything
is sorted; the output depends only on the two revisions and the workspace. When the whole
table is over `budget_tokens`, it is stored under a `text` handle (as the table for
`format: text`, as the `sous-chef-diff/1` JSON document for `format: json`), and the
answer is the table cut to the budget with `full diff h:...: N rows, B bytes (...)` and
the `slice` hint.

The fixture repository of `tests/release_diff.rs` (two commits, a tag `v1`; the snapshot
`tests/snapshots/release_diff__release_diff_text.snap`; recipe rows cut here):

```
recipe	kind	name	change	value	from	to
-	summary	-	-	meta-up: 4 recipes compared (recipes your layers (meta-mine) append or use): 3 changed (1 renamed or moved), 0 added, 0 removed, 1 unchanged; your files requiring files of the layer: 1, 1 with changes; bbappends affected: 3 (1 no longer applies, 1 applies to a different version, 1 source changed under a patch); 1 more: 0 recipe changed, 0 now applies, 1 unchanged; 1 class changed	-	-
-	revisions	meta-up	-	from v1 (cdcd020d327c) to HEAD (3e56cf071d6c)	cdcd020d327c	3e56cf071d6c
-	layer files	-	-	bb 2 changed 3 added 3 removed; inc 3 changed 0 added 0 removed; bbclass 1 changed 0 added 0 removed; patch 0 changed 1 added 0 removed	-	-
hello	bbappend	meta-mine:recipes-core/hello/hello_1.0.bbappend:1	no longer applies	applied to hello_1.0.bb (version 1.0) at v1; at HEAD the recipe is hello_1.1.bb (version 1.1); it is pinned to version 1.0: rename it (or use hello_%.bbappend) if it should still apply; BitBake stops on a bbappend that matches no recipe (a warning only with BB_DANGLINGAPPENDS_WARNONLY, before Walnascar)	meta-up:recipes-core/hello/hello_1.0.bb:1	-
hello	bbappend	meta-mine:recipes-core/hello/hello_%.bbappend:1	applies to a different version	its % now applies to version 1.1 (hello_1.1.bb) instead of 1.0 (hello_1.0.bb): check the append against the new version; its patches now apply to different source; it sets SRC_URI, do_install, which changed in the recipe; source: SRC_URI[sha256sum] = "aaaa" -> "bbbb"; version 1.0 -> 1.1 (file name)	meta-up:recipes-core/hello/hello_1.0.bb:1	meta-up:recipes-core/hello/hello_1.1.bb:1
hello	bbappend patch	file://fix.patch	-	patch meta-mine:recipes-core/hello/hello/fix.patch:1; the source it applies to changed: check it; a patch named fix.patch was added to the recipe upstream: the fix may have been upstreamed; check whether this one is still needed	-	meta-mine:recipes-core/hello/hello_%.bbappend:1
widget	bbappend	meta-mine:recipes-core/widget/widget_git.bbappend:1	source changed under a patch	still applies to widget_git.bb; the source its patches apply to changed: check them; source: SRCREV = "1111111111111111111111111111111111111111" -> "2222222222222222222222222222222222222222"	meta-up:recipes-core/widget/widget_git.bb:1	meta-up:recipes-core/widget/widget_git.bb:1
...
hello	file	recipes-core/hello/hello_1.1.bb	renamed	recipes-core/hello/hello_1.0.bb -> recipes-core/hello/hello_1.1.bb; version 1.0 -> 1.1	meta-up:recipes-core/hello/hello_1.0.bb:1	meta-up:recipes-core/hello/hello_1.1.bb:1
hello	file	conf/distro/elsewhere.inc	not compared	not in the layer at this revision: not compared	meta-up:recipes-core/hello/hello_1.0.bb:7	meta-up:recipes-core/hello/hello_1.1.bb:7
hello	var	DEPENDS =	changed	"zlib" -> "zlib openssl"	meta-up:recipes-core/hello/hello.inc:2	meta-up:recipes-core/hello/hello.inc:2
hello	function	do_install	changed	4 lines -> 5 lines; first difference at code line 3: "... hello ${D}${bindir}" -> "... hello ${D}${bindir}/hello"	meta-up:recipes-core/hello/hello_1.0.bb:17-20	meta-up:recipes-core/hello/hello_1.1.bb:19-23
hello	inherit	inherit pkgconfig	added	-	-	meta-up:recipes-core/hello/hello_1.1.bb:17
hello	SRC_URI entry	file://${@bb.utils.contains(...)}	added	-	-	meta-up:recipes-core/hello/hello_1.1.bb:9-13
hello	SRC_URI entry	file://fix.patch	added	-	-	meta-up:recipes-core/hello/hello_1.1.bb:9-13
...
-	class	classes/autotools.bbclass	changed	1 statement differs; inherited directly by compared hello; inherited by your meta-mine:recipes-app/app/app_1.0.bb:5	meta-up:classes/autotools.bbclass:1	meta-up:classes/autotools.bbclass:1
note: statement-level diff: statements as written, not evaluated; a tree is the .bb and what it requires inside the layer (each revision's BBFILES, no BBMASK); classes listed once
...
```

The CLI: `sous-chef diff LAYER FROM [TO] [--recipe PN] [--all] [--format text|json]
[--output FILE]` prints the whole table (`text`), or the whole diff as JSON, with the time
on stderr; exit status 0 when the diff was produced, 2 when it could not be (a refused or
unknown revision, an unknown layer). A revision starting with `-` must follow `--` to
reach the validation, which refuses it. JSON top-level fields: `schema`
(`"sous-chef-diff/1"`), `layer`, `layer_path` (relative to the repository's top level),
`from` and `to` (`given`, `commit`, `uncommitted`: the count of changed or untracked
files of the layer for the working tree, else null), `selection`, `summary` (`line`,
`compared`, `changed`, `unchanged`, `added`, `removed`, `renamed`, `included_files`,
`appends` by severity, `classes_changed`), `appends` (`append`, `recipe`, `severity`,
`detail`, `from_recipe`, `to_recipe`, `patches` (`entry`, `file`, `upstream`), `touches`,
`source_changes`), `recipes` (`name`, `kind`, `status`, `own_appended`, `reason`,
`from_file`, `to_file`, `from_version`, `to_version`, `moved`, `tree_from`, `tree_to`,
`changes` (`kind`, `group`, `name`, `change`, `old`, `new`, `detail`, `from`, `to`),
`whitespace_only`, `cosmetic_files`, `source_changes`), `classes` (`path`, `change`,
`statements`, `inherited_by`, `own_users`), `layer_files` (`kind`, `added`, `removed`,
`changed`), `notes`. The fixture's JSON is
`tests/snapshots/release_diff__release_diff_json.snap`.

**On the live workspace** (live-environment.md; `scratch/sous-chef.toml`, own layers
`meta-kiss` and `meta-kiss-ti`; release build, 2026-11-17), OE-core's `meta` and meta-arm
against their wrynose checkouts (`33a6bd6c`, `673e8d7c`), working tree as `to`:

| Comparison | Time | Output |
|---|---|---|
| `meta yocto-6.0.3` (default selection) | 0.2 s | 7.1 KB, 44 lines |
| `meta yocto-5.0` (scarthgap to wrynose, default selection) | 0.6 s | 51 KB, 288 lines |
| `meta yocto-6.0 HEAD --all` (949 recipes) | 0.5 s | 278 KB |
| `meta yocto-5.0 --all` (986 recipes) | 0.9 s | 3.3 MB |
| `meta-arm yocto-5.0 --all` (83 recipes) | 0.1 s | 0.7 MB |

So `all` stays a tool call (the table goes to a handle). The default selection on
yocto-kiss picks `dropbear`, `packagegroup-core-boot` (`IMAGE_INSTALL` of `kiss-image`),
`ncurses` (`DEPENDS` of `sl`), `gnutls` (`gnutls-native` in `u-boot-kiss`), and the
includes of `u-boot-kiss_git.bb` (`u-boot-common.inc`, `u-boot.inc`) and of
`dogbonedark.conf`; from yocto-6.0.3 it shows the CVE patches added to `dropbear` and
`gnutls`, and `CVE_PRODUCT` moved from `u-boot.inc` to `u-boot-common.inc` with a new
value as one changed statement. yocto-kiss's only bbappend (`meta-kiss-ti`'s
`linux-kiss_%.bbappend`) appends its own `meta-kiss` recipe, so no upstream bbappend is
affected there. What the static view cannot see: classes used through `inherit ${VAR}`
(`KERNEL_CLASSES`) have no users listed, and a recipe that only a `virtual/` provider or a
package name (not the `PN`) names is not selected by default.

### `sigs`
Why a task re-ran (x4): a digest of `bitbake-diffsigs`, BitBake's own comparison of two
task signatures. Never builds anything.

| Arg | Type | Notes |
|---|---|---|
| `recipe` | string? | with `task`: the two latest signatures of that task (`bitbake-diffsigs -t <recipe> <task>` finds them in the stamps directory and the sstate cache) |
| `task` | string? | `compile` or `do_compile` |
| `sigfile1`, `sigfile2` | string? | instead of `recipe` and `task`: two signature files (a path inside the workspace or build directory, of letters, digits and `_ . + - /` only, so an sstate `*.siginfo` name with `:` cannot be given) |
| `budget_tokens` | int? | default 250 (about 1 KB) |

Signature files are zstd-compressed JSON on every supported release (bitbake-semantics.md
"Task signatures"), and sous-chef has no zstd decoder, so it does not read them: it runs
`bitbake-diffsigs -c never -t <recipe> <task>` (or `-c never <file1> <file2>`) through the
build environment's runner, stores the whole output under a `text` handle and digests it.
Rows `kind item detail loc`:

- `task`: what re-ran and why in one line: `its own inputs changed (basehash)`, or `its own
  inputs did not change; a dependency's did, by way of <dep> -> <dep>` (the recursion
  `bitbake-diffsigs` followed to the root cause);
- `via`: each dependency on that path, outermost first, with its old and new hash (first
  10 hex digits);
- `cause`: the root-cause changes, deepest first (at most 6, then `+N more (limit=6)`):
  a variable or function value changed (the word diff `"a [-old-] {+new+}"`, or the line
  counts of a unified diff, and, when its lines were changed in place (as many `-` as `+`),
  the words added and removed: `value changed: 1 line added, 1 removed; words added:
  --disable-dict`, as for a real `EXTRA_OECONF:append` on curl, whose value BitBake prints
  as a unified diff because it starts with a newline) with the **pointer of the assignment that sets it now**
  (the recipe the task belongs to is evaluated for the default machine and distro; the
  applied operation holding a word the change added, else the last applied one), a
  variable dependency added or removed, the task's variable list changed, a file checksum
  changed or added, a task dependency added or removed, a taint (a forced or invalidated
  task), a change of the ignore lists, or a dependency whose signature could not be found
  (recursion stopped). A `basehash` row only when nothing else at that level explains it.
  `(pn)` follows the item when the change is in a dependency.
- When there is nothing to compare: `only one signature: nothing to compare` (BitBake's
  `Only one matching sigdata file found`), `no signature found`, `the two signatures do not
  differ`, or the failure's `ERROR:` lines, with a note on how many signature files the
  stamps directory holds for the task.

A note gives the command and the handle (`slice handle=h:...` reads the full comparison).
Without a usable runner (bitbake disabled, no build environment, or `bitbake-diffsigs` not
in the runner's allowlist) the tool runs nothing: one `task ... not compared` row, why, the
stamps count, and the command to run by hand.

Hand-written fixture output (`tests/fixtures/outputs/sigs/diffsigs-hello-compile.out`,
fixture workspace, `recipe: "hello", task: "compile"`):

```
kind	item	detail	loc
task	hello:do_compile	re-ran: its own inputs did not change; a dependency's did, by way of hello:do_configure	-
via	hello:do_configure	hash 6f1c2e0a9b -> 0a1b2c3d4e	-
cause	EXTRA_OECONF (hello)	value changed: "--disable-nls {+--with-mine+}"	meta-mine:recipes-core/hello/hello_1.1.bbappend:6
cause	do_configure (hello)	value changed: 1 line added, 0 removed	meta-core:classes-recipe/autotoolsish.bbclass:30
note: from `bitbake-diffsigs -c never -t hello do_compile` (BitBake's own comparison; nothing was built); full output h:... (563 bytes): slice handle=h:...
```

### `pkgdata`
Which package ships a file, and which recipe made a package (x4), from the package data
an existing build left in `PKGDATA_DIR` (bitbake-semantics.md "Package data"). Never
builds; reads plain files only.

| Arg | Type | Notes |
|---|---|---|
| `mode` | `"find_path"` \| `"list_pkg_files"` \| `"lookup_recipe"` \| `"list_pkgs"` \| `"package_info"` | default `find_path`; the `oe-pkgdata-util` spellings (`find-path`, ...) are accepted |
| `path` | string? | `find_path`: a path or glob on the target (`fnmatch` rules: `*` crosses `/`) |
| `package` | string? | `list_pkg_files`, `lookup_recipe`, `package_info`: a runtime name (renamed, e.g. `libfoo2`), a recipe-space name or an `RPROVIDES` name, looked up as `oe-pkgdata-util` does (`runtime-rprovides/`, then `runtime-reverse/`, then `runtime/`); a note says which it was |
| `recipe` | string? | `list_pkgs` |

`PKGDATA_DIR` is the evaluated configuration's value when it is static and exists, else
`bitbake-getvar --value PKGDATA_DIR` when a build environment is available (bitbake
enabled), else the first `<build>/tmp*/pkgdata/<MACHINE>` (or the only machine directory
there); a note says which was used. A directory outside the workspace and build directory
is not read (an error says so; the `log` tool has the same limit). With no package data the
answer is one row `pkgdata none: nothing has been packaged in this build directory` and a
note that `do_packagedata` of a built recipe writes it.

- `find_path`: rows `path package recipe loc`, every `FILES_INFO` path of every
  `runtime/` file matching the glob; a renamed package shows `(runtime name ...)`; `loc` is
  the recipe's applied `FILES:<package>` operations (the recipe evaluated for the default
  machine and distro, at most 8 recipes per call), else its `.bb`.
- `list_pkg_files`: rows `path size package`, sorted; a note names the recipe (`.bb`
  pointer) and `PKGSIZE`.
- `lookup_recipe`: rows `package recipe version loc` (`[PE:]PV[-PR]`, the `.bb` of that
  version when the layers have it, else the one BitBake would use).
- `list_pkgs`: rows `package runtime-name packaged size` from the recipe's `PACKAGES` in
  the package data; `no (empty)` when there is no `.packaged` marker.
- `package_info`: rows `package field value loc`: recipe and version (with the `.bb`),
  package version `[PKGE:]PKGV[-PKGR]`, runtime name (`PKG:`), size, packaged, and where set
  `RDEPENDS`, `RRECOMMENDS`, `RSUGGESTS`, `RPROVIDES`, `RREPLACES`, `RCONFLICTS`,
  `LICENSE`, `SECTION`, `SUMMARY`, the number of files.

More than 40 rows: the first 40, `+N more (limit=40)`, and every row under a `text` handle
(`slice handle=h:...`). Values are decoded as `oe.packagedata.read_pkgdatafile` decodes them
(Python `unicode_escape`), `FILES_INFO` too, so an escaped file name (`café "notes".txt`)
is shown as it is on the target; `oe-pkgdata-util find-path` and `list-pkg-files` parse the
undecoded JSON instead and show such a name escaped (or fail on a `"`).

`find_path` with `path: "/usr/bin/hello"` on a fixture copy with the recorded tree
`tests/fixtures/outputs/pkgdata/` and `PKGDATA_DIR` in `bitbake.conf`:

```
path	package	recipe	loc
/usr/bin/hello	hello	hello	FILES:hello meta-core:conf/bitbake.conf:27, meta-core:recipes-core/hello/hello.inc:20
note: PKGDATA_DIR = <ws>/build/tmp/pkgdata/qemux86 (from the evaluated configuration)
note: loc is the recipe's FILES:<package> assignment (evaluated for the default machine and distro), else its .bb; paths are FILES_INFO as do_package recorded them
```

## MCP resources

- `sous-chef://help`: this document condensed, generated from the tool registry and the
  argument schemas (each tool's description and arguments).
- `sous-chef://rules`: the lint rule catalogue, rows `id severity scope flags`. `severity`
  is the effective one (`[lint.severity]` applied); `flags` is the description from
  lint-rules.md, prefixed with `[releases A to B]` for a rule limited to some releases and
  `[off by default]` for a rule that runs only when selected. A last line lists
  `[lint] disable`, if set.

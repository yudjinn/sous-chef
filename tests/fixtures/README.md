# Test fixtures

Synthetic BitBake metadata for tests, plus recorded real output. No real poky checkout is
used anywhere except the opt-in live tests (`SOUS_CHEF_LIVE`, docs/live-environment.md). Helpers are in `tests/common/mod.rs` (`fixture_path`, `fixture_ws`,
`fixture_config`, `temp_ws`, `files_under`).

Rules for changing fixtures:

- Tests assert exact lines. If you edit a fixture file, run the whole suite and update this
  README in the same change. Prefer adding a new file over editing an existing one.
- `.gitattributes` marks `tests/fixtures/**` as `-text`: no line-ending conversion, so CRLF,
  BOM and trailing-whitespace fixtures keep their bytes. Do not let an editor strip them.
- Every file is UTF-8 (`fixtures_smoke.rs` checks). Tests that need invalid UTF-8 or
  truncated input build it in memory from these files.
- Modern colon override syntax everywhere except `ws/meta-legacy/` and `syntax/legacy.bb`.

Layout:

```
fixtures/
  ws/        a complete synthetic workspace (build dir + four layers)
  syntax/    one small file per statement form, plus malformed files (parser tests)
  lint/      per-rule bad/good fixtures (see lint/README.md)
  kas/       a kas project and what kas checkout adds to it
  fakebb/    fake bitbake, devtool, bitbake-layers, kas and init scripts (runner and job tests)
  outputs/   recorded build output: hand-written logs, live/ (real output), envdump/
  hooks/     Claude Code hook events, hand-written, and captured/ (real events)
```

---

## `ws/`: the workspace

`ws/sous-chef.toml`: `build_dir = "build"`, `writable_layers = ["meta-mine"]`,
`[bitbake] enabled = false`. `common::fixture_config()` builds the same configuration
directly (absolute `build_dir`, `work_dir` = `ws/`).

### Layers

| Layer | In `BBLAYERS` | Collection | `BBFILE_PRIORITY` | `LAYERDEPENDS` | `LAYERSERIES_COMPAT` | Writable |
|---|---|---|---|---|---|---|
| `meta-core` | yes, 1st | `core` | 5 | (none) | `wrynose` | no |
| `meta-vendor` | yes, 2nd | `vendor` | 6 | `core` | `scarthgap wrynose` | no |
| `meta-mine` | yes, 3rd | `mine` | 10 | `core` | `kirkstone scarthgap wrynose` | yes |
| `meta-legacy` | **no** | `legacy` | 7 | `core` | `dunfell` | no |

Every `layer.conf` does `BBPATH .= ":${LAYERDIR}"` and sets
`BBFILES += "${LAYERDIR}/recipes-*/*/*.bb ${LAYERDIR}/recipes-*/*/*.bbappend"` (written over
two lines with a `\` continuation) and `BBFILE_PATTERN_<c> = "^${LAYERDIR}/"`.
`meta-core/conf/layer.conf` also sets `LAYERVERSION_core = "15"` and
`LAYERSERIES_CORENAMES = "wrynose"`.

The workspace is a **Wrynose** (6.0) workspace: discovery reports
`wrynose (LAYERSERIES_CORENAMES)`. `meta-mine` is the multi-release writable layer: it is
linted against kirkstone, scarthgap and wrynose (`Layer::lint_releases`); the other layers
against wrynose alone. `meta-legacy` deliberately claims `dunfell`, older than the floor.
The recipes were written before the release was set and are not all Wrynose-clean: for
example `libfoo_2.0.bb` sets `S = "${WORKDIR}/git"` and `app_0.1.bb` reads
`${WORKDIR}/app.c`, where Styhead and later unpack into `UNPACKDIR`. The second is a
finding (`hygiene/release-syntax`, `app_0.1.bb:22`) in `sous-chef lint` on `ws/` and in the
audit snapshots.

`meta-legacy` exists only for legacy-syntax tests and the "no build dir" fallback: discovery
without a build directory (every `conf/layer.conf` under `ws/`) finds **four** layers;
discovery through `build/conf/bblayers.conf` finds **three**.

### `build/conf/bblayers.conf`

- `BBPATH = "${TOPDIR}"` (the build directory; so `BBPATH` is build dir, meta-core,
  meta-vendor, meta-mine after the layer confs).
- `LAYERS_ROOT = "${TOPDIR}/.."`, a variable assigned in the same file.
- `BBLAYERS ?=` over five physical lines (lines 10-14) with `\` continuations, entries
  `${LAYERS_ROOT}/meta-core`, `${TOPDIR}/../meta-vendor`, `${TOPDIR}/../meta-mine`.
  `${TOPDIR}` is the build directory, `ws/build`.

### `build/conf/local.conf`

| Line | Statement | Exists to test |
|---|---|---|
| 2 | `MACHINE ??= "qemux86"` | default machine is **qemux86** (weak default, so a target machine overrides it) |
| 3 | `DISTRO ?= "testdistro"` | default distro is **testdistro** |
| 9 | `EXTRA_OECONF:append:pn-hello = " --enable-local-debug"` | `pn-` override from a conf file; `org/recipe-change-in-local-conf` |
| 13 | `DISTRO_FEATURES += "wayland"` | `+=` on a variable that `testdistro.conf:5` defaults with `?=`. local.conf is parsed first, so the `?=` sees a value and does nothing: final `DISTRO_FEATURES` contains only `wayland`, not `acl ipv4 ipv6 usbhost`. `override/plus-equals-on-default` |

### `meta-core` (stands in for OE-Core)

- `conf/bitbake.conf`: paths (`TMPDIR`, `WORKDIR`, `S`, `B`, `D`, `bindir`...), `PACKAGES`,
  `FILES:${PN}` / `FILES:${PN}-dev` / `FILES:${PN}-doc`, and the override machinery:
  - line 33 `TARGET_ARCH ?= "i686"`, `TRANSLATED_TARGET_ARCH ??= "${TARGET_ARCH}"`,
    line 35 `MACHINEOVERRIDES ?= "${MACHINE}"`, `DISTROOVERRIDES ?= "${DISTRO}"`,
    `CLASSOVERRIDE ?= "class-target"`, `LIBCOVERRIDE ?= ":libc-glibc"`;
  - line 39 `OVERRIDES = "${TARGET_OS}:${TRANSLATED_TARGET_ARCH}:pn-${PN}:layer-${FILE_LAYERNAME}:${MACHINEOVERRIDES}:${DISTROOVERRIDES}:${CLASSOVERRIDE}${LIBCOVERRIDE}:forcevariable"`
    (the OE-Core shape);
  - `FILESPATH = "${@base_set_filespath(...)}"`: python, never evaluated statically;
  - lines 48-52, in this order: `include conf/site.conf`, `include conf/auto.conf`
    (neither exists: silently ignored), `include conf/local.conf` (resolves through
    `BBPATH` to `build/conf/local.conf`), `include conf/machine/${MACHINE}.conf`,
    `include conf/distro/${DISTRO}.conf`.
- Expected `OVERRIDES` (`FILE_LAYERNAME` is the collection whose `BBFILE_PATTERN` matches the
  recipe, verified by T3.4 in `cooker.py` `get_layername`; `core` for hello, `mine` for app):
  - MACHINE=qemux86: `linux:i686:pn-<pn>:layer-<c>:qemux86:testdistro:class-target:libc-glibc:forcevariable`
  - MACHINE=boardx: `linux:arm:pn-<pn>:layer-<c>:socx:boardx:testdistro:class-target:libc-glibc:forcevariable`
- `conf/machine/qemux86.conf`: `TARGET_ARCH = "i686"`, `MACHINE_FEATURES`, no
  `MACHINEOVERRIDES` change.
- `conf/machine/boardx.conf`: `SOC_FAMILY = "socx"`, line 7 `MACHINEOVERRIDES =. "socx:"`
  (so `MACHINEOVERRIDES` = `socx:boardx`), `TARGET_ARCH = "arm"`,
  `PREFERRED_PROVIDER_virtual/kernel ?= "linux-boardx"` (no such recipe exists).
- `conf/distro/testdistro.conf`: line 5 `DISTRO_FEATURES ?= "acl ipv4 ipv6 usbhost"`;
  line 8 `PREFERRED_PROVIDER_virtual/foo ?= "libfoo-alt"`; line 9
  `PREFERRED_VERSION_libfoo-alt ?= "1.%"` (`%` wildcard; matches 1.0); line 13
  `INHERIT += "sanityish"`.
- `classes/base.bbclass`: always inherited globally. In `classes/` although the layer's
  compat is post-4.1, so it is an `org/class-location` hit. Contains: a top-level
  `def base_set_filespath(path, d)` block, `addtask fetch` (bare name), `addtask X after Y`
  for unpack/patch/configure/compile/install/build, flags (`do_fetch[dirs]`,
  `do_install[dirs]`, `do_build[noexec]`), python and shell `base_do_*` functions,
  `EXPORT_FUNCTIONS do_fetch do_unpack do_patch do_configure do_compile do_install`,
  `addhandler base_eventhandler` with `[eventmask]` and its python body.
- `classes-recipe/autotoolsish.bbclass` (a recipe class): header comment documenting the
  interface; `?=` interface `EXTRA_OECONF`, `AUTOTOOLS_SCRIPT_PATH`, `EXTRA_AUTORECONF`;
  `DEPENDS:prepend` with a trailing space; `autotoolsish_do_{autoreconf,configure,compile,install}`;
  `addtask autoreconf after do_patch before do_configure`;
  `EXPORT_FUNCTIONS do_autoreconf do_configure do_compile do_install`.
- `classes-global/sanityish.bbclass` (a global class, via `INHERIT`): `?=` variable,
  `addhandler sanityish_check`, python handler.
- Classes therefore exist in all three directories: `classes/` (base), `classes-recipe/`
  (autotoolsish), `classes-global/` (sanityish). `meta-legacy/classes/oldclass.bbclass` is
  a fourth, outside `BBLAYERS`. No class is shadowed across layers.
- Configuration parse order (verified by T3.4 in `cookerdata.py` `parseConfigurationFiles`,
  asserted in `tests/eval_replay.rs`): `bblayers.conf`; `layer.conf` of meta-core,
  meta-vendor, meta-mine; `meta-core/conf/bitbake.conf`; `build/conf/local.conf`;
  `conf/machine/<MACHINE>.conf`; `conf/distro/testdistro.conf`; then `base.bbclass` and
  `sanityish.bbclass` (BitBake inherits `["base"] + INHERIT`, base first).

### Recipes

| Recipe file | PN / PV | Notes |
|---|---|---|
| `meta-core/recipes-core/hello/hello_1.0.bb` | hello 1.0 | `require hello.inc`; `LIC_FILES_CHKSUM`; `SRC_URI[sha256sum]` (flag) |
| `meta-core/recipes-core/hello/hello_1.1.bb` | hello 1.1 | as 1.0, plus line 5 `SRC_URI += "file://hello.patch"`, `PACKAGECONFIG ??= "greeting"`, `PACKAGECONFIG[greeting]` |
| `meta-core/recipes-core/libfoo/libfoo_2.0.bb` | libfoo 2.0 | `PROVIDES = "virtual/foo"` (line 6); git `SRC_URI` with `branch=main`, `SRCREV`, `S = "${WORKDIR}/git"`; `PACKAGECONFIG ??= "zlib"`; `PACKAGECONFIG[zlib] = "--with-zlib,--without-zlib,zlib,zlib"`; `PACKAGECONFIG[ssl] = "--with-ssl,--without-ssl,openssl"` (not selected); `RDEPENDS:${PN}-dev = "${PN}"` |
| `meta-vendor/recipes-vendor/libfoo-alt/libfoo-alt_1.0.bb` | libfoo-alt 1.0 | second `PROVIDES = "virtual/foo"` (line 6); tarball with `sha256sum` |
| `meta-mine/recipes-app/app/app_0.1.bb` | app 0.1 | `LICENSE = "CLOSED"`; `DEPENDS = "libfoo"`; `${@...}` in `APP_GREETING` (line 8) which `EXTRA_OEMAKE` uses; `RDEPENDS:${PN} = "libfoo"`; anonymous `python () {` (lines 13-16); top-level `def`; shell `do_compile`; `fakeroot do_install()` (lines 25-29); `python do_report()`; `addtask report after do_compile before do_build` |
| `meta-legacy/recipes-legacy/oldapp/oldapp_1.0.bb` | oldapp 1.0 | legacy syntax, see below (not in `BBLAYERS`) |

- Shared `.inc`: `meta-core/recipes-core/hello/hello.inc`, required by both hello
  versions. It holds `SUMMARY`, `DESCRIPTION`, `HOMEPAGE`, `SECTION`, `LICENSE = "MIT"`,
  `DEPENDS = "virtual/foo"`, line 9 `SRC_URI = "https://example.org/releases/hello-${PV}.tar.gz"`,
  `inherit autotoolsish` (line 11), `EXTRA_OECONF = "--disable-nls"`,
  `do_install:append()` (lines 15-18) and `FILES:${PN} += "${sysconfdir}/hello.conf"`
  (idiomatic OE; package-override `+=`). No version data in the `.inc`.
- `hello` versions: 1.0 and 1.1, both in meta-core. With no `PREFERRED_VERSION_hello`, the
  preferred version is **1.1** (highest version, same layer).
- `meta-core/recipes-core/hello/files/hello.patch`: has `Upstream-Status: Pending`.

### bbappends

| bbappend | Layer (priority) | Matches |
|---|---|---|
| `meta-vendor/recipes-core/hello/hello_%.bbappend` | vendor (6) | `hello_1.0.bb` **and** `hello_1.1.bb` |
| `meta-mine/recipes-core/hello/hello_1.1.bbappend` | mine (10) | `hello_1.1.bb` only |
| `meta-mine/recipes-app/orphan/orphan_9.%.bbappend` | mine (10) | **nothing** (no `orphan` recipe anywhere): `org/append-no-match`, `layers show=appends` |
| `meta-legacy/recipes-legacy/oldapp/oldapp_%.bbappend` | legacy (7) | `oldapp_1.0.bb` (only when meta-legacy is a layer) |

Application order for hello 1.1: the vendor append, then the mine append (lower priority
first; verified in the BitBake source, `collect_bbfiles` / `get_file_appends` /
`multi_finalize`, by T1.4 and T3.4; not checked against real `bitbake-layers show-appends`
output).

`hello_%.bbappend` (vendor):
- line 2 `FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"` (files in
  `meta-vendor/recipes-core/hello/hello/`: `vendor.patch`, `boardx.cfg`);
- line 4 `SRC_URI:append = " file://vendor.patch"`;
- line 5 `SRC_URI:append:boardx = " file://boardx.cfg"`: active only for MACHINE=boardx;
- line 7 `EXTRA_OECONF:append:boardx = " --enable-boardx"`;
- **line 9 `EXTRA_OECONF:append:boradx = " --enable-typo"`: the deliberately misspelled
  machine override.** `boradx` matches no machine, distro, class or package; the closest
  known name is `boardx`. Expected in `overrides show=dead` and `override/dead`.

`hello_1.1.bbappend` (mine):
- line 2 `FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"` (file
  `meta-mine/recipes-core/hello/hello/mine.patch`);
- line 4 `SRC_URI:append = " file://mine.patch"`;
- line 6 `EXTRA_OECONF:append = " --with-mine"`;
- lines 8-10 `do_install:append()`.

### Expected answers (for tool and evaluator tests)

- `SRC_URI` for hello 1.1 has operations from: `hello.inc:9` (`=`), `hello_1.1.bb:5`
  (`+=`), `hello_1.1.bb:6` (`[sha256sum]` flag), vendor append lines 4 and 5, mine append
  line 4. With MACHINE=qemux86 the vendor line 5 is inactive; with MACHINE=boardx it applies.
  Expected final value (qemux86):
  `https://example.org/releases/hello-1.1.tar.gz file://hello.patch file://vendor.patch file://mine.patch`
  (boardx adds ` file://boardx.cfg` before ` file://mine.patch`, since appends apply in
  parse order).
- `EXTRA_OECONF` for hello 1.1: `?=` in bitbake.conf (sets `""`, later shadowed by the
  `.inc`), `?=` in autotoolsish (no effect: already set), `=` in `hello.inc`, `:append:pn-hello` from local.conf (active for
  hello), `:append:boardx` and `:append:boradx` from vendor, `:append` from mine.
- `item recipe=hello name=do_install:append` matches two statements: `hello.inc:15-18`
  and `meta-mine:recipes-core/hello/hello_1.1.bbappend:8-10` (listed, not guessed).
- Providers of `virtual/foo`: `libfoo` (meta-core) and `libfoo-alt` (meta-vendor). The
  selected one is **libfoo-alt**, by
  `meta-core:conf/distro/testdistro.conf:8`. `hello` depends on `virtual/foo`.
- Reverse dependencies of `libfoo` (by name): `app` (`DEPENDS` line 4 and
  `RDEPENDS:${PN}` line 11 of `app_0.1.bb`).
- Python-dependent values: `APP_GREETING` and `EXTRA_OEMAKE` in app, `FILESPATH` everywhere.
  `app` has anonymous python.

### `meta-legacy` (not in `BBLAYERS`)

Pre-honister `_` override syntax. True positives for `override/legacy-syntax`:

- `oldapp_1.0.bb`: `DEPENDS_append`, `EXTRA_OECONF_append_qemux86`, `SRC_URI_remove`,
  `RDEPENDS_${PN}`, `FILES_${PN}-dev +=`, `ALLOW_EMPTY_${PN}-doc`, `do_install_append()`,
  `pkg_postinst_${PN}()`.
- `oldapp_%.bbappend`: `FILESEXTRAPATHS_prepend :=`.
- `classes/oldclass.bbclass`: `do_configure_prepend()`, `EXTRA_OECONF_append_class-native`.

Names with `_` that must **not** be flagged: `SRCREV_machine`, `do_install()`,
`${PACKAGECONFIG_CONFARGS}` (a reference), and in `conf/layer.conf`
`BBFILE_COLLECTIONS`, `BBFILE_PATTERN_legacy`, `BBFILE_PRIORITY_legacy`,
`LAYERDEPENDS_legacy`, `LAYERSERIES_COMPAT_legacy`.

### Known lint hits in `ws/` (non-exhaustive; M4 writes the exact expected file)

`org/recipe-change-in-local-conf` (local.conf:9), `override/plus-equals-on-default`
(local.conf:13), `hygiene/release-syntax` (app_0.1.bb:22), `org/append-no-match` (orphan
append); with `--all` also `org/class-location` (meta-core/classes/base.bbclass),
`org/append-wildcard-broad` (vendor append line 4) and `override/dead` (vendor append line
9). `meta-legacy` is not in `BBLAYERS`, so it is no workspace layer and lint does not check it
(a path in it as the target is refused as outside the workspace layers).
`app` gets no `hygiene/missing-metadata` finding: its only source is a `file://` entry, so
no `HOMEPAGE` is asked of it. The exact list `sous-chef lint` gives on `ws/` (default and
`--all`) is in docs/lint-rules.md, "Findings on the fixture workspace"; `tests/audit.rs`
audits `ws/` itself (`meta-legacy` is its layer present but not enabled).

Not under `fixtures/`: the release diff tests (`tests/release_diff.rs`) build their git
repository (an upstream layer with two commits and a tag `v1`, and a writable layer with
bbappends) in a temporary directory from text in the test, with fixed dates so the commit
hashes in the snapshots are stable; tests that need a changed workspace (`temp_ws`,
`temp_ws_release`) copy `ws/` to a temporary directory first, so `ws/` itself never
changes.

---

## `syntax/`: parser fixtures

One small file per statement form in docs/bitbake-semantics.md. Each file is valid
BitBake unless its name starts with `malformed-`.

| File | Form(s) |
|---|---|
| `assign-set.bb` | `=`; empty value; extra spaces around the operator; no spaces at all |
| `assign-default.bb` | `?=` twice (second has no effect) |
| `assign-weak-default.bb` | `??=` twice (last one wins) |
| `assign-immediate.bb` | `:=` capturing a value that later changes |
| `assign-append.bb` | `+=`, including on a variable never set before |
| `assign-prepend.bb` | `=+` |
| `assign-append-dot.bb` | `.=` |
| `assign-prepend-dot.bb` | `=.` |
| `assign-flags.bb` | `VAR[flag]` with `=`, `+=`, `?=`; task flags; `SRC_URI[sha256sum]`; `PACKAGECONFIG[ssl]` |
| `assign-quotes.bb` | single quotes, double quotes, the other quote inside, `#` inside a value |
| `assign-names.bb` | names with `${PN}`, `:`, `-`, `/`, `.`, `+` (`FILES:${PN}-dev`, `PREFERRED_PROVIDER_virtual/kernel`, `MY.DOTTED+NAME`) |
| `export.bb` | `export VAR = ...`, bare `export VAR`, `export` with `?=` and extra spaces |
| `unset.bb` | `unset VAR`, `unset VAR[flag]` |
| `overrides.bb` | `:append`, `:prepend`, `:remove`, `:qemux86`, `:append:qemux86`, `:qemux86:append`, `:qemux86 +=`, chained `:pn-hello:class-target`, `FILES:${PN}-dev:append` |
| `continuation.bb` | a value over four physical lines (lines 1-4), then a normal line 5 |
| `comments.bb` | comment lines (with and without a space, indented), blank lines |
| `function-shell.bb` | shell function with nested `{ }` text and an `if`; `name () {` with a space |
| `function-override.bb` | `do_install:append()`, `do_install:prepend:class-target()`, `do_configure:qemux86 () {` |
| `function-python.bb` | `python do_report()`, `python do_report:append()` |
| `function-anonymous.bb` | `python () {` and `python __anonymous() {` |
| `function-fakeroot.bb` | `fakeroot do_bar()`, `fakeroot python do_baz()` |
| `pydef.bb` | two top-level `def` blocks (one with a blank line inside the body), then an assignment at column 0 ending the block |
| `inherit.bb` | `inherit a`, `inherit a b`, `inherit ${VAR}`, `inherit_defer ${@...}`, `inherit_defer a` |
| `include.bb` | `require` (plain and with `${BPN}`), `include`, `include ${@...}` |
| `include-all.conf` | `include_all` |
| `addtask.bb` | `addtask` with `after`+`before`, bare name without `do_`, several deps after one keyword, `before` repeated, only `after`; `deltask` with and without `do_` |
| `addhandler.bb` | `addhandler`, its `[eventmask]` flag and python body |
| `exportfuncs.bbclass` | class functions `exportfuncs_do_*` and `EXPORT_FUNCTIONS` |
| `crlf.bb` | CRLF line endings, including a continuation and a function body |
| `no-final-newline.bb` | last line has no `\n` |
| `bom.bb` | starts with a UTF-8 BOM |
| `legacy.bb` | legacy true positives (lines 2-10) and false positives (lines 13-20: `SRCREV_machine`, `BBFILE_PRIORITY_core`, `LAYERSERIES_COMPAT_core`, `${PACKAGECONFIG_CONFARGS}`, `PACKAGE_BEFORE_PN`, `do_install()`) |
| `malformed-unterminated-function.bb` | `do_broken() {` (line 2) never closed: the `}` on line 4 is indented, so the body runs to end of file and swallows line 5 |
| `malformed-bad-continuation.bb` | line 2 ends in `\` followed by three spaces (does not continue; diagnostic); line 4 ends in `\` at end of file |
| `malformed-garbage.bb` | valid line 1; garbage line 2; assignment with no name (line 3); unterminated quote (line 4); stray `}` (line 5); valid line 6 the parser must recover to |

---

## `kas/`: a kas project (T9.3)

Modelled on bootlin/yocto-kiss (`wrynose`). Tests copy it with `tests/kas_support` and
create `.git` in the copy, so `project/` is the repository root kas uses.

- `project/`: the project repository. `kas/base.yaml` (like `kiss.yaml`: `distro`,
  `target`, repos `bitbake` (layer disabled), `openembedded-core` (`branch`),
  `meta-vendor` (`commit`, the repository itself as layer), `proj` (no `url`: the project,
  layer `meta-proj`), a `local_conf_header`), `kas/common.yaml` (included by `base.yaml`;
  `local_conf_header` and `bblayers_conf_header` entries `common`), `kas/board.yaml` (the
  machine file: `machine: board1`, repo `proj-bsp` with layer `meta-proj-bsp`),
  `kas/base.lock.yaml` (pins `openembedded-core`). Own layers `meta-proj` (distro
  `projdistro`, one recipe) and `meta-proj-bsp` (machine `board1`).
- `checkout/`: what `kas checkout kas/base.yaml:kas/board.yaml` adds, copied over the
  project: `openembedded-core/meta` (collection `core`, `LAYERSERIES_CORENAMES = "wrynose"`),
  `meta-vendor`, and `build/` with `CACHEDIR.TAG` and `conf/bblayers.conf` /
  `conf/local.conf` written exactly as kas writes them for these files.

`fakebb/kasbin/kas` is the fake kas (see its header); `lint/org/generated-conf-edit/` holds
the rule's `good` (the generated files) and `bad` fixtures; `hooks/bash_kas_build.json` is a
hand-written `kas build` event.

---

## `fakebb/`: a fake BitBake

Never a real BitBake. `bin/bitbake` prints its program name, working directory, `BUILDDIR`
and argv, then behaves as the `FAKEBB_*` variables say (`FAKEBB_OUTPUT` a file to print,
`FAKEBB_STDERR`, `FAKEBB_SLEEP`, `FAKEBB_PIDFILE`, `FAKEBB_IGNORE_TERM`, `FAKEBB_EXIT`; see
its header); `bitbake-layers`, `bitbake-getvar`, `devtool` and `oe-pkgdata-util` link to
it. `oe-init-build-env` sets `BUILDDIR`; `init-with-env` also reads per-test settings from
`$BUILDDIR/fakebb.env`; `init-build-env` is a fake bitbake-setup script ending in `set --`,
like the real one; `kasbin/kas` is a fake kas (see its header). Used by
`tests/bitbake_env.rs`, `bitbake_jobs.rs`, `bitbake_kas.rs` and `var_authoritative.rs`.
`bin/bitbake-diffsigs` (x4) also links to it; `tests/sigs.rs` runs it with
`FAKEBB_OUTPUT` / `FAKEBB_STDERR` set to a file of `outputs/sigs/`.

## `outputs/`: recorded output

- Top level: hand-written task logs, run files and console output in the shapes of
  docs/bitbake-semantics.md "Build output" (`log.do_<task>.<pid>`, `run.do_compile.48213`,
  `bitbake-*.out`), for `bitbake::logs` and the `log` tool.
- `envdump/`: a trimmed real `bitbake -e curl` from the live environment, for the
  `bitbake -e` parser and authoritative `var` (its README says how it was trimmed).
- `sigs/` (x4): `bitbake-diffsigs` output, **hand-written** from BitBake's
  `compare_sigfiles` and `bin/bitbake-diffsigs` (docs/bitbake-semantics.md "Task
  signatures"), for `tests/sigs.rs`: a variable change one dependency down
  (`diffsigs-hello-compile.out`), Kirkstone-style task names three dependencies down
  (`diffsigs-kirkstone-patch.out`), a task's own inputs, a taint
  (`diffsigs-files.out`), and the "only one signature" error (`diffsigs-only-one.err`).
- `pkgdata/qemux86/` (x4): a package data tree **hand-written** in the format of
  OE-core's `emit_pkgdata` (generated once with Python's `unicode_escape` encoder and
  `json.dumps(sort_keys=True)`, as OE-core writes it; docs/bitbake-semantics.md "Package
  data"), for `tests/pkgdata.rs`: `hello` (four packages, `hello-dev` empty without
  `.packaged`, `hello-doc` with the escaped name `café "notes".txt`) and `libfoo`
  (`libfoo` renamed `libfoo2` by `PKG:`, `RPROVIDES` `libfoo-compat`, 47 files), with the
  `runtime-reverse/` and `runtime-rprovides/` symlinks. Tests copy it into a temporary
  `build/tmp/pkgdata/qemux86`.
- `live/`: real output recorded on the live environment (kas, bitbake-setup, the
  verification's fetches, hook events built for real files, the differential report), with
  the live prefix replaced by `{{LIVE}}`; its README lists every file and command.

## `hooks/`: hook events

One `PostToolUse` event per file, hand-written for the handlers' tests (`hook_*.rs`):
Grep and Read events on `ws/` files, Bash events (a large bitbake failure, `kas build`,
`grep -rn` / `rg -n` and the forms the grep handler must skip, another command), Edit and
Write, an unknown tool. `captured/` holds real events from a Claude Code session (Grep
content mode and Read only; its README says how they were captured). No Bash or Edit event
has been captured from a live session.

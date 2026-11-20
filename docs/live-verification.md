# Live verification (T9.2)

sous-chef run side by side with BitBake on a real Wrynose workspace, 2026-11-17. Each item
says what was run, what sous-chef said, what BitBake said, and a verdict: **confirmed**,
**corrected** (a doc was wrong and is fixed), **bug** (the code is wrong; reproduction
given) or **still open**.

## Setup

- Workspace: the kas build of [docs/live-environment.md](live-environment.md)
  (`{{LIVE}}/yocto-kiss/build`, kas files `kas/kiss.yaml:kas/dogbonedark.yaml` plus the lock
  file). BitBake 2.18.0 (`6e260eb6`), kas 5.5, openembedded-core `33a6bd6c`, meta-arm
  `673e8d7c`, yocto-kiss `cc621c5d` (all `wrynose`). `MACHINE=dogbonedark`, `DISTRO=kiss`.
  The bitbake-setup build (`{{LIVE}}/bitbake-builds/kiss-wrynose-dogbonedark/build`) was only
  read: another agent was running BitBake in it.
- sous-chef: this branch, `cargo build --release`, run with `--config
  {{LIVE}}/scratch/sous-chef.toml` (`build_dir = "../yocto-kiss/build"`, `writable_layers =
  ["meta-kiss", "meta-kiss-ti"]`) and `XDG_CACHE_HOME={{LIVE}}/scratch/cache`. MCP tools were
  called through `sous-chef serve` over stdio by a small client
  (`{{LIVE}}/scratch/mcp.py`); a second config with `[output] budget_tokens = 200000` was
  used to see whole tables.
- BitBake commands: `cd {{LIVE}}/yocto-kiss && env -u NETRC_FILE ../venv/bin/kas shell
  kas/kiss.yaml:kas/dogbonedark.yaml -c '<command>'` (`{{LIVE}}/scratch/kasrun`). Raw output
  is in `{{LIVE}}/runs/verify/` (and `{{LIVE}}/runs/` for what T9.1 captured); the committed,
  path-scrubbed samples are in `tests/fixtures/outputs/live/` (see its README).
- Automated checks: `tests/live_verify.rs`, run with
  `SOUS_CHEF_LIVE={{LIVE}} cargo test --test live_verify -- --ignored`. 7 pass; the 7 named
  `live_bug_*` fail on purpose, one per bug below.

**BitBake tasks run** (only these): `bitbake -c fetch curl` (after `bitbake -n -c fetch
curl` listed only `do_recipe_qa` and `do_fetch`), and `bitbake -c fetch broken-fetch` of
the scratch recipe (after the same dry run check). Nothing compiled. Everything else was
parse-only (`bitbake -e`, `bitbake-getvar`, `bitbake-layers show-*`, `kas dump`).

Two side effects to know about: `bitbake -n` is not network-free: the dry run of curl's
fetch downloaded the uninative shim (`build/downloads/uninative/.../x86_64-nativesdk-libc-5.1.tar.xz`,
7 MB) before BitBake disabled uninative ("Your host glibc version (2.44) is newer than
that in uninative (2.43)"). And the scratch fetch of an `.invalid` URL did not stop at the
DNS lookup: OE-core's `MIRRORS` made wget ask `downloads.yoctoproject.org/mirror/sources/`
and `mirrors.edge.kernel.org/yocto-sources/` for `broken-fetch-1.0.tar.gz` (both 404). A
scratch recipe that must touch nothing should set `MIRRORS = ""` and `PREMIRRORS = ""`.

## 1. Discovery and release — confirmed (one naming difference)

`status` and `layers show=list` against `conf/bblayers.conf` and `bitbake-layers
show-layers`:

| BitBake (`show-layers`) | path (as BitBake prints it) | priority | sous-chef layer | priority | writable |
|---|---|---|---|---|---|
| meta-arm | `build/../meta-arm/meta-arm` | 5 | meta-arm | 5 | no |
| arm-toolchain | `build/../meta-arm/meta-arm-toolchain` | 5 | meta-arm-toolchain | 5 | no |
| meta-kiss | `build/../meta-kiss` | 9 | meta-kiss | 9 | yes |
| meta-kiss-ti | `build/../meta-kiss-ti` | 9 | meta-kiss-ti | 9 | yes |
| core | `build/../openembedded-core/meta` | 5 | meta | 5 | no |

Same layers, same order (`BBLAYERS` order), same priorities. The `${TOPDIR}/../`-relative
paths kas writes resolve (sous-chef shows them canonicalised, BitBake as written).
`release wrynose (LAYERSERIES_CORENAMES)`; BitBake has `LAYERSERIES_CORENAMES="wrynose"`.
Writable: the two `meta-kiss*` layers, as configured.

Difference, not a bug: **BitBake 2.18's `show-layers` names a layer by its collection**
(`bblayers/query.py` prints the `bbfile_config_priorities` collection name), sous-chef by
its directory name. `arm-toolchain` / `meta-arm-toolchain` and `core` / `meta` differ;
pointers use the directory name (`meta:recipes-support/...`). Worth a line in mcp-tools.md
(`layers`): "the layer name is the directory name; BitBake's `show-layers` prints the
collection".

Test: `live_discovery_matches_show_layers` (passes).

## 2. Index — confirmed for metadata; bug for non-metadata files

`sous-chef index` on the workspace: 1755 files (bb 1034, bbappend 13, bbclass 240, conf 90,
inc 378) in 1.17 s from nothing, 12 MB of index; a no-change refresh 0.11 s. Nothing
panicked. (BitBake: `Parsing of 1033 .bb files complete (0 cached, 1033 parsed). 2085
targets, 435 skipped`; the 1034th `.bb` is the template below.)

**Parser diagnostics on real metadata** (every `.bb`, `.bbappend`, `.bbclass`, `.inc` outside
`files/` and `.conf` under `conf/` except `conf/ccache.conf`; more than 1600 files): six,
all correct: "whitespace after the trailing `\`" warnings, each a real `\ ` at end of line
(checked with `grep -E '\\[[:space:]]+$'`: autoconf_2.73.bb:2, perl_5.42.0.bb:321-322,
mc_4.8.33.bb:50-51, libxml-sax-perl_1.02.bb:4).

When this was first checked there were three more, "not modelled by sous-chef" notes for
real directives: `addpylib` in `meta/conf/layer.conf:147` and `meta-arm/conf/layer.conf:21`,
`addfragments` in `meta/conf/bitbake.conf:822`. Both are statement kinds of their own now,
with no diagnostic: `addfragments` since w5-fix-eval2, and replayed (it loads
`OE_FRAGMENTS` and the built-in `machine:MACHINE distro:DISTRO` fragments, which is how
`bitbake-config-build enable-fragment distro/kiss machine/dogbonedark` sets `DISTRO` and
`MACHINE` in bitbake-setup builds; bitbake-semantics.md, "Configuration fragments"), and
`addpylib` since w7-fix (it changes no variable).

No parser construct is missing for OE-core or meta-arm. Test:
`live_parser_has_no_diagnostics_on_real_metadata` (passes).

**Bug B1: data files are indexed and linted as BitBake metadata.** The scanner takes every
`.conf` and `.inc` in a layer. OE-core ships 35 `.conf` files outside `conf/` that are
configuration files installed by recipes (`wpa-supplicant/wpa_supplicant.conf`,
`lighttpd/lighttpd.conf`, `busybox/files/mdev.conf`, the systemd `*.conf` drop-ins ...),
plus `conf/ccache.conf` (ccache's own configuration) and `files/wic/common.wks.inc` (a wic
kickstart include). `lint --all` reports **422 `hygiene/parse-error` errors in 29 of these
files** (all of the rule's findings). Reproduction: `sous-chef lint --all --rule
hygiene/parse-error` on any OE-core checkout; expected none; actual 422. Likely fault:
`src/index/scan.rs` (which files are metadata) or the rule. A `.conf` is metadata when it
is reached as `conf/...` through `BBPATH` or by `include`/`require`; an `.inc` when it is
included. The same scan also takes `meta/lib/bblayers/templates/example.bb`, which no
`BBFILES` glob collects, and `org/recipe-path` then reports it. Test:
`live_bug_data_files_are_not_metadata` (fails).

## 3. bbappends and shadowed recipes

`layers show=appends` and the index (`query::appends_for`, the order the replay applies
them in) against `bitbake-layers show-appends`.

- **Matching — confirmed.** The same 20 recipes get the same bbappends (`linux-kiss_7.0.bb`
  ← meta-kiss-ti's `linux-kiss_%.bbappend`; meta-arm's `linux-yocto%.bbappend` on five
  linux-yocto recipes; `trusted-firmware-a_%` on four versions; `optee-os_%` twice on two
  versions; `u-boot_%`, `musl_%`, `systemd_%`, `systemd-boot_%`, `edk2-firmware_%`,
  `libts_%`, `ts-sp-smm-gateway_%`). BitBake also marks most of them `(skipped)`
  (`COMPATIBLE_MACHINE` and friends), which sous-chef did not model then (known; since w6 /
  w8 / w11 the skip notes do, see "After w10 and w11").
- **Bug B2: application order of same-priority bbappends.** For `optee-os_4.9.0.bb` and
  `optee-os_git.bb` BitBake applies `recipes-security/optee-ftpm/optee-os_%.bbappend` then
  `recipes-security/optee/optee-os_%.bbappend`; sous-chef the other way round.
  `cooker.collect_bbfiles` sorts each glob's matches as strings (`sorted(glob.glob(f))`,
  `-` 0x2d < `/` 0x2f, so `optee-ftpm/` < `optee/`); `resolve::recipe_files`
  (`src/workspace/resolve.rs`, `m.sort()` on `Vec<PathBuf>`) sorts paths component by
  component, where `optee` < `optee-ftpm`. Minimal reproduction: one layer with
  `recipes-x/a/foo_%.bbappend` and `recipes-x/a-b/foo_%.bbappend` (glob
  `recipes-*/*/*.bbappend`) appending to `foo_1.0.bb`: BitBake order `a-b` then `a`,
  sous-chef `a` then `a-b`. Any value both set comes out differently. Fix: sort by the
  path's string (bytes). Test: `live_bug_appends_order_vs_show_appends` (fails on
  optee-os only).
- **Bug B3: a `BBFILES_DYNAMIC` bbappend is reported as matching nothing.**
  `meta-arm:dynamic-layers/meta-tpm2/recipes-tpm/tpm2-tools/tpm2-tools_5.7.bbappend` is
  collected only through `BBFILES_DYNAMIC` (`tpm2:` ...) when a layer with collection `tpm2`
  is present; here none is, so BitBake never sees it (not in `show-appends`). `layers
  show=appends` lists it as `no-match` with "BitBake stops with a fatal error on these".
  Expected: not listed, or listed as "not collected (BBFILES_DYNAMIC, tpm2 absent)".
  `org/append-no-match` gets it right (it looks only at bbappends `BBFILES` collects,
  `query::orphan_appends`). Fault: `src/mcp/tools/layers.rs` `appends` (every indexed
  bbappend without an edge is called `no-match`; it should use the same orphan query). Test: `live_bug_dynamic_layer_append_reported_as_no_match` (fails).
- **Shadowed — confirmed.** `show-overlayed`: "No overlayed files found."; `layers
  show=shadowed`: "no recipe or class file is provided by more than one layer". Test:
  `live_no_shadowed_recipes` (passes).

## 4. Evaluation spot checks on curl

`var` for `curl` (static, `MACHINE=dogbonedark DISTRO=kiss` from `local.conf`) against
`{{LIVE}}/runs/bitbake-e-curl.out`:

| Variable | Static = BitBake? | Flagged? | Note |
|---|---|---|---|
| `SRC_URI` | **equal** (1175 bytes, compared byte for byte through the `value` handle) | not flagged since x1 (re-checked in x7; before: flagged as python-dependent) | `OVERRIDES`, which decides its override variants, is now evaluated (its inline python is in the evaluated subset, said in a note); the generic warnings about opaque anonymous python remain |
| `FILE_LAYERNAME` | **equal** (`core`) | no | |
| `TMPDIR` | **equal** (`build/tmp`) | no | |
| `PREFERRED_PROVIDER_virtual/kernel`, `.../bootloader` | equal | no | |
| `DEPENDS` | differs | yes | BitBake: `pkgconf-native autoconf-native automake-native libtool-native libtool-cross gcc-cross-arm virtual/compilerlibs virtual/libc libidn2 openssl zlib virtual/update-alternatives`; static: `pkgconf-native ${@get_autotools_dep(d)} ${@get_base_dep(d)} ${@bb.utils.contains('PTEST_ENABLED', ...)}`. The PACKAGECONFIG and update-alternatives words come from anonymous python (named in the warnings) |
| `PACKAGECONFIG` | differs only by `${@bb.utils.filter('DISTRO_FEATURES', 'ipv6', d)}` → `ipv6` | yes | |
| `OVERRIDES` | differs | yes | see below |
| `S`, `WORKDIR`, `T` | differ (`MULTIMACH_TARGET_SYS` is python: `armv7a${@...}` vs `armv7at2hf-neon`) | yes | the rest of each path is equal |
| `MACHINEOVERRIDES` | differs (`${@bb.utils.contains('TUNE_FEATURES', ...)}` ×5 + `${@['', 'ti-soc:ti33x:'][...]}dogbonedark` vs `armv7a:ti-soc:ti33x:dogbonedark`) | yes | |
| `DISTROOVERRIDES` | differs (`${@d.getVar('DISTRO') or ''}` vs `kiss`) | yes | |

Every difference is flagged. Classification: all are "python not evaluated", except
`TCOVERRIDE` (bug B5). Test: `live_curl_static_values_equal_bitbake` (passes).

**Python in `OVERRIDES`.** BitBake's list for curl has 13 entries:
`linux-gnueabi:arm:pn-curl:layer-core:armv7a:ti-soc:ti33x:dogbonedark:kiss:class-target:toolchain-gcc:libc-glibc:forcevariable`.
Statically known as clean entries: `pn-curl`, `layer-core`, `class-target`, `libc-glibc`,
`forcevariable` (5 of 13); `ti33x` appears only by accident (see B4); `linux-gnueabi`,
`arm`, `armv7a`, `ti-soc`, `dogbonedark`, `kiss` are inside python; `toolchain-gcc` is
missing (B5). `overrides show=conditional` for curl: 11 active, 1327 inactive, with the
warning "OVERRIDES contains python and unresolved TCOVERRIDE; its real entries are not all
known, so active/inactive may be wrong". Example of a wrong state, correctly warned about:
`TARGET_CC_ARCH:append:arm` (`time64.inc:32`) is shown inactive; BitBake applies it
(`-D_TIME_BITS=64 -D_FILE_OFFSET_BITS=64` is in `TARGET_CC_ARCH`).

**Bug B4: `overrides show=chain` (and the known-override set) split `OVERRIDES` on `:`
inside `${@...}`.** The chain for curl lists 18 "entries" such as `', '',
d)}${@bb.utils.contains('TUNE_FEATURES', 'armv4', 'armv4`, `ti33x`, `']['ti-soc` and
`ti33x' != '']}dogbonedark`. Expected: each python piece shown once as unknown
("${@...} (python, not evaluated)"), never split. The same fragments become "known
overrides": `overrides show=dead` suggests `'',` as the closest name to `armv4`, and lists
the package `curl-ptest` (in `PACKAGES` only through `${@bb.utils.contains('PTEST_ENABLED',
'1', '${PN}-ptest', '', d)}`) as a dead override with closest `'curl-ptest',`.
Reproduction: `overrides recipe=curl show=chain` on any OE-core workspace with an ARM tune.
Fault: the chain attribution / known-override collection in `src/eval/overrides.rs` or
`src/mcp/tools/overrides.rs` (splitting the expanded text on `:` / whitespace without
skipping `${@...}`). Test: `live_bug_overrides_chain_splits_python` (fails).

**Bug B5: `toolchain/${TOOLCHAIN}` is not inherited, so `TCOVERRIDE` is missing.**
OE-core Wrynose's `base.bbclass:36` has `inherit_defer toolchain/${TOOLCHAIN}`;
`TOOLCHAIN ??= "${PREFERRED_TOOLCHAIN}"` → `gcc`, and `var TOOLCHAIN recipe=curl` shows
`final: "gcc"`. But the value carries the python flag (its variants `:class-native` ...
depend on `OVERRIDES`), so the replay records `inherit_defer toolchain/${TOOLCHAIN}` as
`RefProblem::Python` and never parses `classes/toolchain/gcc.bbclass`. Every recipe then
lacks `TCOVERRIDE = "toolchain-gcc"` (so `OVERRIDES` keeps a literal `${TCOVERRIDE}`, and
every `var` final line ends in "(unresolved, left as written: TCOVERRIDE)", even for
`SRC_URI`, which never references it), and lacks `CC`, `CXX`, `OBJDUMP` ... from that
class. (Line 35, `toolchain/${TOOLCHAIN_NATIVE}-native`, is followed.) Expected: follow a
reference whose expansion has no `${@` in its own text, or at least when the python flag
comes only from override uncertainty. Fault: `src/eval/replay.rs` `expand_ref` /
`inherit`. Test: `live_bug_toolchain_class_not_inherited` (fails).

**`override/dead` on the `meta-kiss*` layers: no findings** (`lint meta-kiss --rule
override/dead`, `lint meta-kiss-ti ...`): these layers have no conditional operation (only
`:append` / `:prepend` without condition), so there is nothing to judge; true negative. On
OE-core with `--all`: 92 findings, all at info level (the list is partly known); a sample
(`linux-gnux32`, `mingw32`, `nios2`, `arc`, `mipsarchn64el`) are real overrides of
architectures and `TARGET_OS` values no machine of this workspace uses: noise, as
expected. `overrides show=dead recipe=curl` adds 288 `tune-*` rows (`TUNE_FEATURES:tune-cortexa8`
...): those are variables read by name (`${TUNE_FEATURES:tune-${DEFAULTTUNE}}`), not
conditions; false positives of the tool (the lint rule does not see them: it only looks at
the recipe's own files).

## 5. Provider selection — confirmed; `-native` known gap

`deps kind=providers` against `bitbake -e <name>` (`verify/providers-bitbake.out`):

| Name | sous-chef | BitBake |
|---|---|---|
| `virtual/kernel` | `linux-kiss 7.0`, "selected by PREFERRED_PROVIDER_virtual/kernel" (`meta-kiss-ti:conf/machine/dogbonedark.conf:22`); also lists linux-dummy, linux-yocto(-dev,-rt,-tiny) | `PREFERRED_PROVIDER_virtual/kernel="linux-kiss"`, `FILE=.../meta-kiss/recipes-kernel/linux/linux-kiss_7.0.bb`, `PV="7.0"` |
| `virtual/bootloader` | `u-boot-kiss git`, selected by `PREFERRED_PROVIDER_virtual/bootloader` (`dogbonedark.conf:27`) | `FILE=.../meta-kiss/recipes-bsp/u-boot/u-boot-kiss_git.bb`, `PV="git"` |
| `virtual/libc` | `glibc 2.43 (PREFERRED_VERSION_glibc = "2.43%")`, selected by `PREFERRED_PROVIDER_virtual/libc` (`tclibc-glibc.inc:13`) | `FILE=.../glibc_2.43.bb`, `PREFERRED_VERSION_glibc="2.43%"`, `PV="2.43+git"` (the recipe sets `PV`; sous-chef's version column is the file name's) |
| `curl-native` | "no recipe in the workspace provides curl-native"; `recipe curl-native` is "not found" | `FILE=.../curl_8.19.0.bb`, `PN="curl-native"`, `CLASSOVERRIDE="class-native"` |

The `-native` miss was the known `BBCLASSEXTEND` gap at the time; closed since: w10
evaluates variants and makes them providers of their name, w11 also of the `PROVIDES`
their class rewrites and beside real recipes of the same name ("Status after the fix
rounds" below). While there: for the native variant
BitBake leaves **`FILE_LAYERNAME` as `None`** (`set cookerdata.py:513 [_parse_recipe]
"None"`, no `FILE_LAYERNAME=` line) and its `OVERRIDES` contains a literal
`layer-${FILE_LAYERNAME}` (recorded in bitbake-semantics.md). Test:
`live_providers_match_bitbake` (passes).

**Bug B6: `deps kind=build` splits `DEPENDS` inside `${@...}`.** For curl it lists the
"dependencies" `'1',`, `'`, `openssl-native',` and `'',`, the last one with provider
"libglvnd 1.7.0 (first of several providers)" (presumably because libglvnd's `PROVIDES`
is four `${@bb.utils.contains(...)}` expressions whose words are split the same way; not
traced further). Expected: the python piece as one unknown row. Reproduction:
`deps recipe=curl kind=build` (curl_8.19.0.bb:175 `DEPENDS:append:class-target =
"${@bb.utils.contains('PTEST_ENABLED', '1', ' openssl-native', '', d)}"`). Fault:
`src/mcp/tools/deps.rs` (word split of the evaluated value) and the static `PROVIDES`
lookup in `src/eval/providers.rs` / the index. Test: `live_bug_deps_build_splits_python`
(fails).

Minor (`deps kind=packageconfig`): the flags `PACKAGECONFIG[doc]` (documentation.conf) and
`PACKAGECONFIG[vardepvalue]` (bitbake.conf:942) are listed as options, and the python
words of `PACKAGECONFIG` as three "selected (no PACKAGECONFIG flag: ignored)" rows
(`${@bb.utils.filter('DISTRO_FEATURES',`, `'ipv6',`, `d)}`). BitBake's base.bbclass does
iterate every flag, but neither adds anything to `PACKAGECONFIG_CONFARGS`; leave out flags
named `doc` and `vardepvalue` and do not split python.

## 6. The environment runner

- **Detection on the kas build — works without a kas mode.** `status`: `bitbake build
  environment found (init-script): {{LIVE}}/yocto-kiss/build, init
  {{LIVE}}/yocto-kiss/openembedded-core/oe-init-build-env`. Sourcing OE-core's
  `oe-init-build-env` with the kas build directory gives a usable environment: the `log`
  tool's `bitbake-getvar -r curl --value T` ran through it (seen in
  `bitbake-cookerdaemon.log`), and `build/conf/` was not touched. Differences from `kas
  shell`: the user's `PATH` and `HOME` are kept, kas's extra `BB_ENV_PASSTHROUGH_ADDITIONS`
  (`SSTATE_DIR DL_DIR TMPDIR ...`) are not set (none of them is set here), and kas does not
  rewrite `conf/` first, so an edited kas file is not seen until the next kas run.
- **Inside `kas shell -c`** (`kasrun 'python3 mcp.py ...'`): `status` says `build
  environment found (initialised): {{LIVE}}/yocto-kiss/build` and `bitbake op=layers` runs.
- **The `bitbake` tool** (init-script mode): `op=layers` ran `bitbake-layers show-layers`
  (succeeded, output = `show-layers`); `op=env target=curl` ran `bitbake -e curl` (3 s
  with a warm cache, 1.87 MB stored as an `env` handle; `slice var=T` returns the 10-line
  block); `op=status` while running shows `state running elapsed 1s`; a second request
  while it runs is refused with "already running (one job at a time)".
- **Cancel — settled: the server survives the cancel briefly, then exits by itself.**
  `op=env target=curl`, then `op=cancel` 0.8 s later: the job is `cancelled`. `ps` during
  the run: the job (`bitbake -e curl`) had its own process group (pgid = its pid); the
  BitBake server (`bitbake-server decafbad ...`, two processes) had been re-parented
  (ppid 3852, not the job) and was in its own session and process group. After the cancel
  both server processes were still running (and still parsing); 3.3 s later the server
  logged "No timeout, exiting." / "Exiting as we could obtain the lock" and was gone. So
  the group kill does not reach the server; it exits on its own because `BB_SERVER_TIMEOUT`
  is unset (it leaves when its client is gone). With `BB_SERVER_TIMEOUT` set (memory
  resident server) it would stay; not tried.
- **Two clients — settled: the second waits.** A `bitbake -e curl` started at
  16:50:51.68 (ended 16:50:55.56); a `bitbake-layers show-layers` started at 16:50:53.13 in
  a second `kas shell`: it printed `NOTE: Reconnecting to bitbake server...`, waited, and
  finished at 16:50:56.05 with the normal output, exit 0. From the source
  (`lib/bb/main.py`): a client that finds the lock taken connects to the running server's
  socket and retries up to 8 times (5 s apart after the fourth), then fails with "Unable
  to connect to bitbake server, or start one"; a long build would therefore make a second
  client fail after about 20 s (not observed).
- **The bitbake-setup build (read only).** With `build_dir` = its `build/` and
  `init_script` = its `build/init-build-env`, `status` says `build environment found
  (init-script) ... init .../build/init-build-env`, lists 7 layers (meta-poky and
  meta-yocto-bsp added) and release wrynose. But no command can run:

  **Bug B7: the init-script wrapper never runs the program with bitbake-setup's
  `init-build-env`.** The wrapper is `source "$1" "$2" >/dev/null && shift 2 && exec "$@"`
  (`src/bitbake/env.rs`, `WRAPPER`). bitbake-setup's script ends with `set --`, which in
  bash 5.3 clears the caller's positional parameters (checked: after `source init-build-env
  x`, `$#` is 0); `shift 2` then fails and `exec` is never reached: exit 1, no output, for
  every command. Reproduction (no BitBake needed): an init script `cd <dir>; set <build>; .
  ./oe-init-build-env; set --`, then `bash -c "$WRAPPER" -- ./init-build-env <build> echo
  ran` prints nothing and exits 1; without the `set --` line it prints `ran`. Fix: save
  the program and its arguments before sourcing (e.g. copy `"${@:3}"` into an array first).
  Workaround today: `init_script` = `openembedded-core/oe-init-build-env` with the
  bitbake-setup `build_dir` (that is all `init-build-env` does). Test:
  `live_bug_wrapper_with_bitbake_setup_init_build_env` (fails).

`var authoritative=true` still answers "bitbake is not available: no initialised build
environment was found" (T5.5 is not in this branch), although `status` reports an
environment; the wording will mislead once T5.5 lands if not changed with it.

## 7. Logs

- **`TMPDIR` — settled: `build/tmp`.** `bitbake-getvar --value TMPDIR` prints
  `{{LIVE}}/yocto-kiss/build/tmp`; `build/tmp/saved_tmpdir` says the same. The yocto-kiss
  README's `build/tmp-glibc` is out of date for Wrynose (live-environment.md already says
  so; now confirmed).
- **Work directory and `T` — confirmed.** `T` = `${WORKDIR}/temp` (`bitbake.conf:419`),
  `WORKDIR` = `build/tmp/work/armv7at2hf-neon-oe-linux-gnueabi/curl/8.19.0`
  (`${BASE_WORKDIR}/${MULTIMACH_TARGET_SYS}/${PN}/${PV}`, no `PR`). After `-c fetch curl`,
  `temp/` holds `log.do_fetch.<pid>` with the `log.do_fetch` symlink, the same for
  `do_recipe_qa`, `run.do_fetch.<pid>` with its symlink, `run.<function>.<pid>` (no
  symlink) for every helper function that ran (`base_do_fetch`, `extend_recipe_sysroot`,
  `fetcher_hashes_dummyfunc`, `sstate_*`, ...), `depsig.do_recipe_qa.<pid>` with a symlink,
  and `log.task_order` (`<timestamp> <task> (<pid>): log.<task>.<pid>`, one line per task).
- **`bitbake-getvar --value` — confirmed:** prints only the value (with kas's and
  BitBake's NOTE lines on stderr / before it); `bitbake::logs::parse_getvar_value` takes the
  last line.
- **The real failed-fetch log through `log`** (`log recipe=broken-fetch task=fetch`, and
  `log` with no argument, which found the same "most recent failure"): 523 bytes, rows
  `recipe broken-fetch`, `version 1.0`, `task do_fetch`, `error (fetch, line 165)
  FetchError: Unable to fetch URL from any source.`, `url
  https://sous-chef-verify.invalid/broken-fetch-1.0.tar.gz`, a `context` line, `+1 more
  errors`, the log pointer and a handle. It names the recipe, the task and the first
  error, and stays under 1 KB: **confirmed**. Took 3.4 s (it ran `bitbake-getvar` for `T`).
  Not in it, though documented in mcp-tools.md ("Digest: ... exit status, ... a pointer to
  the task's definition in the layers"): the exit status and the task definition pointer
  (`base.bbclass` `base_do_fetch`). Also not in it: the actual cause, wget's `unable to
  resolve host address` / `ERROR 404: Not Found` lines (DEBUG-free lines 19-20, 148, 160
  of the log). For a fetch failure those lines are what a user needs; suggest adding the
  last wget/curl error line. On curl's successful fetch log the tool says "no error lines
  recognised; slice the log" (correct).
- **The Bash hook** (`PostToolUse` Bash event, command `cd build && bitbake -c fetch
  broken-fetch 2>&1`, stdout = the real 5643-byte console output): the output is replaced
  by a 716-byte response: "bitbake output (53 lines, 5643 bytes) stored as h:24c167c247",
  then `recipe`, `version`, `task`, `exit 1`, the `FetchError` with the URL, `+1 more
  errors`, the handle; `slice handle=h:24c167c247 failures=true` returns the four error
  blocks (console lines 40-46). **Confirmed.** 8 ms median per call. With the command as
  it was really run (`kas shell ... -c 'bitbake -c fetch broken-fetch'`) the hook does
  nothing: the known miss T9.3 is fixing. Test: `live_bash_hook_digests_real_fetch_failure`
  (passes), `live_failed_fetch_log_detected` (passes).

**The real fetch failure against the hand-written fixture**
(`tests/fixtures/outputs/log.do_fetch.60311`). Differences in the real log
(`tests/fixtures/outputs/live/broken-fetch/log.do_fetch.1484343`, 167 lines):

1. `fetcher_hashes_dummyfunc` runs (two DEBUG lines) between `extend_recipe_sysroot` and
   `do_fetch`.
2. wget is called with long options: `--tries=2 --timeout=100 --output-document=<f>.tmp
   --continue --directory-prefix=<DL_DIR> '<url>' --progress=dot --verbose`, not `-t 2 -T 100
   -O ... -P ...`.
3. Each attempt logs `DEBUG: Fetching <url> using command '...'`, `DEBUG: Fetcher accessed
   the network with the command ...` and `DEBUG: Running export PSEUDO_DISABLED=1; export
   SSH_AUTH_SOCK="..."; export PATH="..."; export HOME="/tmp/tmp..."; /usr/bin/env wget ...`
   (no `ftp_proxy`; the real environment, including the SSH agent socket, is in the log).
4. wget's own output (`--<date>--  <url>`, `Loaded CA certificate ...`, `Resolving ...
   failed: Name or service not known.`, `wget: unable to resolve host address '<host>'`)
   comes right after the `Running` line, not after a `failed with exit code 8, output:`
   line; the `DEBUG: Fetcher failure: Fetch command ... failed with exit code 4, output:`
   line follows it with an empty output.
5. About 120 `DEBUG: For url [...] comparing [...] to [...]` lines (PREMIRRORS and MIRRORS
   matching) and `DEBUG: For url <u> returning <mirror url>`.
6. Two mirrors are tried, `https://downloads.yoctoproject.org/mirror/sources/<file>` and
   `https://mirrors.edge.kernel.org/yocto-sources/<file>` (each `ERROR 404: Not Found.`,
   then `DEBUG: Mirror fetch failure for url ... (original url: ...)`), not
   `sources.openembedded.org`.
7. Before the `Bitbake Fetcher Error: FetchError(...)` line there is a second ERROR:
   `ERROR: Fetcher failure: Fetch command <full command> failed with exit code 4, see
   logfile for output` (exit code 4 = network failure, of the upstream attempt).
8. The log goes on after the errors: `DEBUG: Python function base_do_fetch finished`,
   `DEBUG: Python function do_fetch finished`.

On the console, each task message carries the `<PF> <task>: ` prefix
(`ERROR: broken-fetch-1.0-r0 do_fetch: ...`), then `ERROR: Logfile of failure stored in:
<T>/log.do_fetch.<pid>`, `ERROR: Task (<recipe file>:do_fetch) failed with exit code '1'`
and `Summary: 1 task failed:` with `  <file>:do_fetch` and `    log: <path>`, as
bitbake-semantics.md says; no `Log data follows:` (an ERROR was logged during the task).

## 8. Lint on real layers

**Default rule set on the writable layers** (`sous-chef lint --severity info`, 13 files,
0.41 s): 6 findings, all info. Judged one by one (yocto-kiss means to be exemplary):

| Finding | Verdict |
|---|---|
| `override/shadowed` `meta:recipes-bsp/u-boot/u-boot-common.inc:10` `LIC_FILES_CHKSUM =` replaced by `u-boot-kiss_git.bb:12` | **false positive in practice**: true that the `.inc`'s value is overwritten for u-boot-kiss, but the `.inc` is OE-core's, shared with u-boot and u-boot-tools where it is not shadowed; "remove it" is wrong advice and the pointer is in a layer the user cannot edit |
| same, `u-boot-common.inc:17` `SRCREV =` (u-boot-kiss_git.bb:11) | false positive, same reason |
| same, `u-boot-common.inc:19` `SRC_URI =` (u-boot-kiss_git.bb:8) | false positive, same reason (the recipe rewrites `SRC_URI` on purpose, as its comment says) |
| `hygiene/missing-metadata` `kiss-image.bb:1` no `HOMEPAGE` | false positive: an image recipe has no upstream home page; OE-core's own images have none |
| `org/recipe-path` `kiss-image.bb:1` "in recipes-core/images/, which holds no other recipe; name the directory after the recipe" | false positive: `recipes-*/images/` is the OE convention for images (OE-core `recipes-core/images/`); the rule skips images only for the "no version" check |
| `hygiene/missing-metadata` `linux-kiss_7.0.bb:1` no `HOMEPAGE` | true (info): kernel.org could be named |

No error or warning on `meta-kiss*`. Not reported and arguably worth it: `sl_git.bb` uses
`PV = "1.0+git${SRCPV}"`; `SRCPV` is `""` in Wrynose's bitbake.conf, so `${SRCPV}` is
dead text (a false negative, no rule covers it).

**`--all`** (`sous-chef lint --all --severity info --format json`, 1755 files, 15.1 s, no
panic): 1486 findings, 437 error, 186 warning, 863 info.

| Rule | Count (meta / meta-arm / meta-kiss) | Sample verdict |
|---|---|---|
| `hygiene/parse-error` | 422 / 0 / 0 | all false: data files (bug B1) |
| `override/shadowed` | 291 / 47 / 0 | mixed. True: `python3-pip_26.0.1.bb:3 SECTION` (pypi.bbclass sets it after); plausible: `binutils.inc:122 OBJDUMP` (the deferred toolchain class sets it after). False: `buildtools-tarball.bb:4 LICENSE` "replaced by buildtools-docs-tarball.bb:5" (that recipe requires the other `.bb`; in buildtools-tarball itself it is not shadowed), `gcc-15.3.inc:83 B` (shadowed only in gcc-source). **Bug B8**: the rule judges a shared `.inc` / required `.bb` from one includer's evaluation and reports at the shared file |
| `hygiene/missing-metadata` | 114 / 16 / 2 | all "no HOMEPAGE" (info); false for images, packagegroups, `-native` helpers, `connman-conf`; true for others |
| `org/recipe-path` | 97 / 17 / 1 | mostly true style notes (`libxml2` in `recipes-core/libxml/`, `perlcross` in `perl-cross/`); false: `meta:lib/bblayers/templates/example.bb` (a template, not collected by `BBFILES`; B1) and the image directory (above) |
| `override/remove-in-recipe` | 104 / 10 / 0 | true by the rule's definition; OE-core does this on purpose (`DEPENDS:remove:class-native`) |
| `hygiene/tabs` | 92 / 0 / 0 | true (mixed indentation exists) |
| `override/dead` | 89 / 3 / 0 | info; real overrides of absent architectures (expected noise, wave 4 notes) |
| `org/class-location` | 40 / 5 / 0 | true by the rule (OE-core keeps `classes/` for classes used both ways) |
| `override/plus-equals-with-override` | 31 / 1 / 0 | true by BitBake semantics (`RDEPENDS:${PN}:class-target +=` replaces `RDEPENDS:${PN}` for target), sometimes intended |
| `org/class-export-functions` | 21 / 2 / 0 | false in the sample: `insane.bbclass` `do_qa_sysroot`, `do_qa_patch`, `do_qa_configure` are the class's own tasks (expected noise, wave 4 notes) |
| `hygiene/host-path` | 15 / 0 / 0 | false in the sample: `busybox.inc:207-208` `grep -q -x "/bin/sh" busybox.links.suid` (a quoted string, not a host path); `glibc-package.inc:143 /var` and `initscripts_1.0.bb:106 /var` need a closer look |
| `override/filesextrapaths` | 7 / 5 / 0 | **false: bug B9.** `FILESEXTRAPATHS =. "${FILE_DIRNAME}/systemd:"` (`systemd-boot_259.5.bb:2`, `cross-localedef-native_2.43.bb:20`, ...) is reported as "without `:prepend` (it replaces the search path)"; `=.` prepends at parse time, keeps the rest, has the trailing colon and uses `${FILE_DIRNAME}` in the `.bb` itself. The rule as written in lint-rules.md ("modified without `:prepend` / `:append`") is too broad: `=.` with a trailing `:` (and `.=` with a leading `:`) should pass |
| `override/append-no-space` | 7 / 3 / 0 | false in the sample: `no-static-libs.inc:24 EXTRA_OECONF:append = "${DISABLE_STATIC}"` (`DISABLE_STATIC ?= " --disable-static"` starts with the space), `u-boot_2026.01.bb:26-27 SRC_URI:append:riscv32 = "${SRC_URI_RISCV}"` (value starts with a continuation); harmless: `ptest-python-pytest.bbclass:35` |
| `org/inc-has-version-data` | 4 / 4 / 0 | false in the sample: `common-clang.inc:12 PV`, `u-boot-common.inc:17 SRCREV`, shared on purpose by recipes of one version |
| `org/class-recipe-specific` | 7 / 0 / 0 | false in the sample: `native.bbclass:12 EXCLUDE_FROM_WORLD =`, `gnomebase.bbclass:14/16` (expected noise, wave 4 notes) |
| `hygiene/continuation` | 6 / 0 / 0 | true (all six checked with grep) |
| `override/append-vs-plus-equals` | 5 / 1 / 0 | true (info, style) |
| `hygiene/autorev` | 4 / 0 / 0 | true facts, intended (`linux-yocto-dev`, the sbom-cve-check fragment) |
| `org/duplicate-versions` | 0 / 4 / 0 | true (info) |
| `org/repeated-logic` | 2 / 2 / 0 | true (info) |
| `override/missing-package` | 3 / 0 / 0 | **false: bug B10.** `bitbake.conf:336 FILES = ""` (an empty value; `pkgvarcheck` tests `if d.getVar(var, False)`, so empty is never reported) and `image.bbclass:34-35 RDEPENDS +=` / `RRECOMMENDS +=` (the class sets `PACKAGES = ""` two lines above, so pkgvarcheck skips it; the w4 follow-up exempted image *recipes*, not the class file) |

## 9. Hooks on real files — confirmed

Events built in the shape of `tests/fixtures/hooks/captured/` for OE-core's
`curl_8.19.0.bb` (`tests/fixtures/outputs/live/hooks/`), 20 runs each, release build:

| Event | Median / max | Annotation | Verdict |
|---|---|---|---|
| Grep `do_install`, content mode, over curl's directory and meta-kiss (6 hits) | 4.8 / 7.3 ms | `curl_8.19.0.bb:136 in [do_install:append:class-target 136-138]`, `:140 in [do_install:append:class-nativesdk 140-145]`, `:151 in [do_install_ptest 151-173]`, `meta-kiss:recipes-games/sl/sl_git.bb:27 in [do_install 27-30]`, and one summary line per recipe | correct (ranges checked against the file) |
| Read, whole `curl_8.19.0.bb` | 4.2 / 4.6 ms | "not the whole recipe; these files also contribute: `meta:conf/distro/include/ptest-packagelists.inc` (require)" | correct but odd: that `.inc` is required by `ptest.bbclass`, not by the recipe, while the recipe's five classes are not named |
| Read, lines 120-129 | 3.4 / 3.7 ms | starts inside `[EXTRA_OECONF 117-124]`, ends inside `[fix_absolute_paths 126-134]` | correct |
| Bash (fetch failure) | 8.1 / 13.9 ms | see item 7 | correct |
| Bash, same output under `kas shell ... -c` | 1.5 ms | none | known miss (T9.3) |

All well under the 150 ms budget on this 1755-file workspace.

## 10. Scaffold, dry run only — mostly sensible; one missing warning

`scaffold write=false` into `meta-kiss`:

- `kind=recipe name=curl version=8.19.0` (tarball, autotools): plans
  `meta-kiss:recipes-support/curl/curl_8.19.0.bb` (category taken from OE-core's curl),
  `inherit autotools` (the class exists in the workspace), no `S` line with the note that
  the default `S` assumes `curl-8.19.0/` (right for Wrynose: `S = ${UNPACKDIR}/${BP}`),
  `SRC_URI[sha256sum]`, and the `LIC_FILES_CHKSUM` placeholder. **Missing:** no note that
  OE-core already has `curl_8.19.0.bb` and that the new file, in a layer of priority 9,
  will shadow it (`layers show=shadowed` would list it after the write). For a recipe
  that exists elsewhere the plan should say so and suggest a bbappend.
- `kind=recipe` for a new name without `category`: refused, "ambiguous category in layer
  meta-kiss; ... candidates: recipes-bsp, recipes-core, recipes-games, recipes-kernel":
  correct for this layer.
- `kind=bbappend name=curl set={"PACKAGECONFIG:append": " nghttp2"}`: plans
  `meta-kiss:recipes-support/curl/curl_%.bbappend` with that one line (wildcard, noted).
  Sensible.
- `kind=bbappend name=curl files=["0001-fix.patch"] machine=dogbonedark`: plans
  `curl_8.19.0.bbappend` with `FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"` and
  `SRC_URI:append:dogbonedark = " file://0001-fix.patch"`, and where to copy the patch.
  Correct. (A machine-specific change for dogbonedark would sit better in meta-kiss-ti,
  where the machine is, by yocto-kiss's own layout; no note says so.)

Nothing was written (`find meta-kiss -newer sous-chef.toml` is empty).

## 11. Other *verify* items settled

From bitbake-semantics.md and the hand-off notes, settled from the BitBake 2.18 / OE-core
Wrynose source in the workspace and the release branches of the BitBake clone:

- `BB_DEFER_BBCLASSES`: added by bitbake commit `8e741b2e8` (2025-06-12, "ast/BBHandler:
  Add support for BB_DEFER_BBCLASSES"); absent on branch `2.12`, present on `2.16` and
  `2.18`. First release: **2.16 (Whinlatter)**. (There is no 2.14 branch.)
- `BB_DANGLINGAPPENDS_WARNONLY`: dropped by `fca9c9e3c` (2024-11-21, "cooker: Drop support
  for BB_DANGLINGAPPENDS_WARNONLY"); present on `2.10`, absent on `2.12`. Last release
  with it: **2.10 (Styhead)**.
- Several eligible providers: `taskdata.add_provider_internal` fires `MultipleProviders`
  and adds every eligible file; the runqueue uses `build_targets[name][0]`: **the first
  one**.
- **Corrected:** bitbake-semantics.md said "There is no `PREFERRED_PROVIDERS` variable".
  There is: `cooker.handlePrefProviders` reads `PREFERRED_PROVIDERS` (words
  `providee:provider`) from the configuration into `recipecaches.preferred`;
  `providers.filterProviders` then lets `PREFERRED_PROVIDER_<name>` override it. sous-chef
  ignored `PREFERRED_PROVIDERS` then (a gap; yocto-kiss does not set it); since w10 it reads
  it as BitBake does (bitbake-semantics.md, "Providers and versions").
- `PREFERRED_PROVIDER_*` is read from the configuration datastore only
  (`filterProviders(..., cfgData, ...)`): **confirmed** in the source, and observed (the
  machine conf's value decides `virtual/kernel`).
- OE-core has the `uppercase-pn` QA check (`insane.bbclass:1557`, "PN: %s is upper case,
  this can result in unexpected behavior."): **confirmed**.
- `IMAGE_INSTALL` is read (as that exact name) only by `image.bbclass`,
  `core-image.bbclass` and recorded by `buildhistory.bbclass`: **confirmed** by grep (note
  that `meta-kiss-ti/conf/machine/dogbonedark.conf` sets `IMAGE_INSTALL:append`, which a
  machine conf may do).
- Fetcher checksum and `SRCREV` lookup (`fetch2/__init__.py` `FetchData.configure_checksum`,
  `srcrev_internal_helper`): **confirmed with two refinements**, see bitbake-semantics.md:
  with `name=<n>` the flag is `SRC_URI[<n>.sha256sum]` only (no fallback to
  `SRC_URI[sha256sum]`; the plain name is a fallback only for a `sha256sum=` URL
  parameter), and checksums are looked up only for http(s), ftp(s), sftp, s3, az, crate,
  gs, gomod and npm URLs. `SRCREV` order: `SRCREV_<n>:pn-<pn>`, `SRCREV_<n>`,
  `SRCREV:pn-<pn>`, `SRCREV`, first value not `INVALID`; a `rev=` URL parameter is used
  when none is set and must agree otherwise.
- Wrynose build environment: settled with T9.1's notes and item 6 (bitbake-setup's
  `init-build-env` is `. oe-init-build-env <build>` from OE-core; `oe-init-build-env` works
  for both builds; bug B7 for the wrapper).

**Still open:**

- `bitbake -e` output format (the other agent's item).
- The format of Grep's `-A/-B/-C` context lines, and live Bash / Edit events (need a live
  Claude Code session; the events here are built by hand).
- How `bitbake-layers add-layer` edits `bblayers.conf` (it writes the file; not run, to keep
  the kas build's generated conf as kas wrote it).
- When `python_hatchling` first appeared (not a parse-only question; the OE-core clone has
  the history, not checked).
- The second client's failure after a long wait (needs a long-running first command).
- The server's behaviour on cancel with `BB_SERVER_TIMEOUT` set.

## Bugs, most serious first

| | Bug | Where (believed) | Test |
|---|---|---|---|
| B2 | Same-priority bbappends applied in the wrong order when one directory name is a prefix of another (`optee` / `optee-ftpm`): paths sorted by component, BitBake sorts strings | `src/workspace/resolve.rs` `recipe_files` | `live_bug_appends_order_vs_show_appends` |
| B5 | `inherit_defer toolchain/${TOOLCHAIN}` not followed (python flag from override uncertainty), so `toolchain/gcc.bbclass` and `TCOVERRIDE` are missing for every recipe | `src/eval/replay.rs` `expand_ref` / `inherit` | `live_bug_toolchain_class_not_inherited` |
| B7 | init-script wrapper never runs the program with bitbake-setup's `init-build-env` (`set --` clears `$@`) | `src/bitbake/env.rs` `WRAPPER` | `live_bug_wrapper_with_bitbake_setup_init_build_env` |
| B1 | Non-metadata `.conf` / `.inc` files (and a template `.bb`) indexed and linted: 422 false parse errors on OE-core | `src/index/scan.rs` | `live_bug_data_files_are_not_metadata` |
| B4 | `overrides` splits `OVERRIDES` inside `${@...}` (chain shows fragments; fragments become known overrides; python-only packages reported dead) | `src/mcp/tools/overrides.rs` / `src/eval/overrides.rs` | `live_bug_overrides_chain_splits_python` |
| B6 | `deps kind=build` splits `DEPENDS` (and `PROVIDES`) inside `${@...}`; `'',` "provided" by libglvnd | `src/mcp/tools/deps.rs`, `src/eval/providers.rs` | `live_bug_deps_build_splits_python` |
| B3 | A `BBFILES_DYNAMIC` bbappend whose dynamic layer is absent is shown as `no-match` with "BitBake stops with a fatal error" | `src/mcp/tools/layers.rs` `appends` | `live_bug_dynamic_layer_append_reported_as_no_match` |
| B9 | `override/filesextrapaths` flags `FILESEXTRAPATHS =. "...:"` (a correct prepend) as an error | `src/lint/rules/override_filesextrapaths.rs`, and its row in lint-rules.md | (lint `--all` sample) |
| B10 | `override/missing-package` flags `FILES = ""` and a class that sets `PACKAGES = ""` | `src/lint/rules/override_missing_package.rs` | (lint `--all` sample) |
| B8 | `override/shadowed` reports a shared `.inc` / required `.bb` from one includer's evaluation, with "remove it" advice | `src/lint/rules/override_shadowed.rs` | (lint default on meta-kiss: 3 findings) |
| B11 | `overrides show=dead` lists `VAR:tune-<name>` variables (read by name) as dead overrides (288 rows for curl) | `src/mcp/tools/overrides.rs` | — |
| B12 | `org/recipe-path` and `hygiene/missing-metadata` (HOMEPAGE) flag image recipes in `recipes-*/images/` | `src/lint/rules/org_recipe_path.rs`, `hygiene_missing_metadata.rs` | (lint default on meta-kiss) |
| B13 | `hygiene/host-path` flags a quoted string (`grep -q -x "/bin/sh"`) | `src/lint/rules/hygiene_host_path.rs` | — |
| B14 | `override/append-no-space` flags `:append = "${VAR}"` when `VAR` starts with a space | `src/lint/rules/override_append_no_space.rs` | — |
| B15 | `deps kind=packageconfig` lists the `doc` and `vardepvalue` flags as options and python words as options | `src/mcp/tools/deps.rs` | — |
| B16 | `log` digest lacks the exit status and the task-definition pointer that mcp-tools.md promises, and the wget cause line | `src/bitbake/logs.rs` | — |
| B17 | `scaffold kind=recipe` for a recipe that exists in another layer gives no shadowing note | `src/scaffold/recipe.rs` | — |
| B18 | Every `var` final line carries the `OVERRIDES` unresolved note ("left as written: TCOVERRIDE") even when the variable does not reference it | `src/mcp/tools/var.rs` | — |

Gaps confirmed at the time (known, not bugs): `BBCLASSEXTEND` providers (`curl-native`),
skipped recipes (`COMPATIBLE_MACHINE`; BitBake skips 435), `addfragments`,
`PREFERRED_PROVIDERS`. What became of each: "After the later rounds" below.

## Status after the fix rounds (2026-11-17)

The 18 bugs were assigned to the fix branches `w5-fix-eval`, `w5-fix-lint`, `w5-fix-hooks`
and `w5-fix-eval2` (merged by `e046d5f`). Checked for this section at `8f66f94`: `SOUS_CHEF_LIVE=... cargo test
--release --test live_verify -- --ignored` passes all 14 tests, the seven `live_bug_*`
included, and the lint numbers were re-measured on the kas build without running BitBake
(lint-rules.md, "On a real workspace").

| | Fixed by | Status |
|---|---|---|
| B1 | data files marked in the index (`files.not_collected = 'data'`), left out of search and lint; uncollected recipe files noted | fixed; `hygiene/parse-error` with `--all` 422 -> 3. The 3 left (in `meta:conf/ccache.conf`, ccache's own file, then taken as metadata because it is in `conf/`) went in w6 (an unreferenced `.conf` under `conf/` that does not parse is data: 3 -> 0), and so did lint's 2 `org/recipe-path` findings on `meta:lib/bblayers/templates/example.bb`, which no `BBFILES` glob collects (now 1 finding saying so). Since `lint-dynamic-data` the data files beside a recipe BitBake does not collect count too (meta-oe's `dynamic-layers/` recipes of an absent collection): on the Scarthgap and Kirkstone release workspaces `parse-error` with `--all` 369 -> 0 and 365 -> 0, Wrynose unchanged (lint-rules.md, "On a real workspace") |
| B2 | glob matches sorted as Python strings (`resolve::py_str_cmp`) | fixed; bbappend order agrees for all recipes of the bitbake-setup build (differential.md) |
| B3 | `layers show=appends` evaluates `BBFILES_DYNAMIC` | fixed for the view; since w6 the index folds `BBFILES_DYNAMIC` into `BBFILES` (`resolve::collect`) and the view reads the index |
| B4 | one tokeniser for list values (`parse::words`): python entries of `OVERRIDES` kept whole, never override names | fixed |
| B5 | a reference whose text expands completely but whose value is uncertain is followed and flagged | fixed; flagged values listing `TCOVERRIDE` 156 -> 0 (differential.md) |
| B6 | the same tokeniser in `deps` and the static `PROVIDES` | fixed |
| B7 | the wrappers save the argv before sourcing | fixed |
| B8 | `override/shadowed` does not judge a shared file from one includer | fixed; 338 -> 83 findings. The false positives seen in the re-run (a recipe's `DEPENDS =` "replaced" by the virtual-provider rewrite, then recorded as an assignment) went in w6: the rewrite is its own kind of operation (`OpKind::VirtualProviderRewrite`), 83 -> 65 |
| B9 | `override/filesextrapaths` accepts `=.` / `.=` with the colon | fixed; 12 -> 0 |
| B10 | `override/missing-package` skips empty values and classes without packages | fixed; 3 -> 0 |
| B11 | `overrides show=dead` counts variables read by name (`TUNE_FEATURES:tune-*`) in a note instead of listing them | fixed |
| B12 | images and packagegroups through their static tree (`org/recipe-path`, `hygiene/missing-metadata`) | fixed; the default run on meta-kiss* went from 6 findings to 1 (the true one) |
| B13 | `hygiene/host-path` treats text, patterns, quoted arguments and link targets as data | fixed; 15 -> 0 |
| B14 | `override/append-no-space` judges a leading / trailing reference by the file's own value | fixed; 10 -> 6 |
| B15 | `deps kind=packageconfig` lists only real options | fixed |
| B16 | `log`: exit status from the console log or job output, the downloader's cause row, the definition from a global class | fixed (not re-run against the real fetch failure: that needs BitBake) |
| B17 | `scaffold kind=recipe` notes a recipe of the same `PN` in another layer | fixed |
| B18 | the `OVERRIDES` note only on values with a conditional operation, and worded for what it means | fixed |

Also since this verification: `var authoritative=true` is implemented (T5.5, merged before
the fix rounds); the Bash hook recognises `kas shell ... -c 'bitbake ...'` (T9.3; the live
test `live_bash_hook_digests_real_fetch_failure` covers it since `8f66f94`); a bitbake-setup
build has its MACHINE and DISTRO from the fragments (`addfragments` is replayed; the
`@default` probe of differential.md went from 12 bugs to 0); a no-change refresh takes
19 ms instead of 104 ms (performance.md).

Still open from the list above: the Grep context-line format and live Bash and Edit events
(need a live Claude Code session); how `bitbake-layers add-layer` edits `bblayers.conf`;
the first release with `python_hatchling`; the second client's failure after a long wait;
cancel with `BB_SERVER_TIMEOUT` set. The `bitbake -e` format item is settled (T5.5,
bitbake-semantics.md "Build output"). Known gaps at that point: `BBCLASSEXTEND` providers
(then, since w6, such a name was an error naming the base recipe), skipped recipes (since
w6 / w8 a "would skip" / "may skip" note when the values allow, which then never fired on
yocto-kiss because its `MACHINEOVERRIDES` holds python), `PREFERRED_PROVIDERS`;
`addfragments` was no longer one. `BBCLASSEXTEND` providers and `PREFERRED_PROVIDERS` were
closed later, and the skip notes now fire on yocto-kiss (w10, w11; next sections).

## After the later rounds (w9-final, 2026-11-18)

At the end of the code part of branch `w9-final` (after w6 to w8 and this branch's code
items), with the release build: `SOUS_CHEF_LIVE=... cargo test --release --test live_verify
--test audit --test release_diff -- --ignored` passes (14 live verification tests, the live
audit test and the live release diff test); no BitBake or kas process was started. Lint on
the kas build: default 1 finding, `--all` 625 findings in 1718 files, unchanged since w6
(lint-rules.md, "On a real workspace"); the differential on the existing dumps is unchanged
(differential.md, "After w9-final").

## After w10 and w11 (w11-final, 2026-11-18)

w10 added class-extended variants (evaluated as BitBake makes them, providers of their
name), `PREFERRED_PROVIDERS`, `BBMASK` recomputed from its inputs, and compared the skip
checks with BitBake's values (`tests/skip_live.rs`): 8 recipes BitBake skipped were not
"would skip" there (u-boot, musl, systemd for reasons the checks did not model;
linux-yocto, linux-yocto-dev, edk2-firmware, optee-os 4.9.0 and git for a
`COMPATIBLE_MACHINE` that is not static). w11 modelled the two missing reasons
(`features_check.bbclass` and the `RecipeParsed` virtual-provider skip; bitbake-semantics.md
"Skipped recipes"), made a variant a provider through the `PROVIDES` its class rewrites and
beside a real recipe of its name, and ordered a variant's class against the deferred
inherits as each BitBake branch does. Re-measured at the end of `w11-final` with the
release build, no BitBake or kas process started (outputs in
`{{LIVE}}/runs/w11-final/`):

- Skip comparison (`tests/skip_live.rs`, BitBake's values from the existing dumps): 3 of
  the 8 disagreements remain: linux-yocto 6.18, optee-os 4.9.0 and optee-os git, whose
  `COMPATIBLE_MACHINE` is not static (linux-yocto's `PN` neither, so the virtual-provider
  check cannot decide it either). u-boot and musl are now skipped by the virtual-provider
  check (`PREFERRED_PROVIDER_virtual/bootloader` = u-boot-kiss, `virtual/libc` = glibc),
  systemd by `features_check` (`REQUIRED_DISTRO_FEATURES` systemd and usrmerge missing),
  linux-yocto-dev (`virtual/kernel`) and edk2-firmware (`virtual/bootloader`) by the
  virtual-provider check. Agreement with `show-appends`: 21 of 24 (was 16). With the real
  values 237 recipes are judged skipped (reasons: 83 machine or host, 150
  `features_check`, 12 virtual provider; a recipe can have two), none of them one BitBake parsed and none listed
  by `show-appends` as not skipped. The notes from static values alone: "would skip" for
  13 recipes (w10: 0), none of them one BitBake parsed (the test fails on that), "may skip"
  for 274 (w10: 124), most of them `features_check` notes, because the static
  `DISTRO_FEATURES` of yocto-kiss holds python.
- Differential on the existing dumps (`SOUS_CHEF_DIFF_NO_BITBAKE=1`): 1090 values
  compared, 701 equal (+1 after whitespace), 357 flagged, 31 known gap, 0 bug
  (differential.md, "After w11-final").
- Lint on the kas build (`sous-chef lint --severity info`, run from the kas project
  directory, because in w11 a kas project named in `sous-chef.toml` took its work
  directory, where remote repositories are checked out, from the working directory; since
  w12 it does not, see lint-rules.md "On a real workspace"): default 1 finding in 13
  files; `--all` 625 findings in 1718 files (0 E, 166 W, 459 I), in 14.5 s: unchanged
  (lint-rules.md).
- `SOUS_CHEF_LIVE=... cargo test --release --test live_verify --test audit --test
  release_diff --test skip_live -- --ignored`: all pass (14, 1, 1, 1).

`w12-final` took the kas work directory from the configuration instead of the working
directory, computed the `nativesdk` and multilib `PROVIDES` filters, and made the
`features_check` and `RecipeParsed` checks decide on certain words only. Re-measured with
the release build, no BitBake or kas process started (outputs in
`{{LIVE}}/runs/w12-final/`, `TMPDIR` and `XDG_CACHE_HOME` there too):

- Lint run from `runs/w12-final/` (not the kas project directory) with
  `runs/w12-final/sous-chef-kas.toml`: `--rule org/generated-conf-edit build` 3 warnings
  before (the remote repositories' `BBLAYERS` entries), 0 after; default 1 finding in 13
  files; `--all` 625 findings in 1718 files, the same lines as the w11 run from the project
  directory (lint-rules.md).
- Differential on the existing dumps: 1090 compared, 702 equal (+0 after whitespace), 357
  flagged, 31 known gap, 0 bug; `nativesdk-zlib` `PROVIDES` moved from equal after
  whitespace (flagged) to equal (differential.md, "After w12-final").
- Skip comparison (`tests/skip_live.rs`): from static values alone "would skip" 13, none of
  them a recipe BitBake parsed, and "may skip" 274, unchanged; recipes not decidable with
  BitBake's values 36 (42 before: the `nativesdk` recipes' `PROVIDES` is no longer
  "changed by python"); agreement with `show-appends` 21 of 24.
- The four live tests above: all pass (14, 1, 1, 1).

## After the x-batch and its cleanup (x7-cleanup, 2026-11-18)

The six x branches (python helpers, migration report and SARIF, `lint --fix`, the build
views, multiconfig, the benchmark) and the cleanup round. Re-measured with the release
build of `x7-cleanup`, no BitBake or kas process started (outputs in
`{{LIVE}}/runs/x7-cleanup/`, `TMPDIR` and `XDG_CACHE_HOME` there too, `[bitbake] enabled =
false`):

- Differential on the existing dumps (`SOUS_CHEF_DIFF_NO_BITBAKE=1`): 1090 compared, 950
  equal, 109 flagged (74, and 35 whose literal part differs), 31 known gap, 0 bug; the same
  totals as at the end of x1 (differential.md).
- Skip comparison (`tests/skip_live.rs`): 1034 recipes collected, none failed to evaluate;
  with BitBake's values 260 judged skipped (74 `COMPATIBLE_HOST`, 56 `COMPATIBLE_MACHINE`,
  155 `features_check`, 13 virtual provider, 2 `SKIP_RECIPE`; a recipe can have two), 5
  not decidable statically; from static values alone "would skip" 259 and "may skip" 6
  (w12-final: 13 and 274; the static `DISTRO_FEATURES` of yocto-kiss held python that x1
  evaluates).
- Lint (`sous-chef lint`, from the yocto-kiss directory with the kas files in
  `sous-chef.toml`): default 5 findings in 13 files (1 W, 4 I: `linux-kiss` has no
  `SUMMARY` and no `HOMEPAGE`, `u-boot-kiss_git.bb` sets no `PV`, `sl_git.bb` has
  `${SRCPV}` in `PV` and skips the `ldflags` QA check: the four facts of the benchmark's
  t09), then the coverage note; `--all` 649 findings in 1718 files (0 E, 156 W, 493 I).
  lint-rules.md, "On a real workspace".
- `curl`'s `SRC_URI` (section 4) is no longer flagged: equal and unflagged since x1.
- The four live tests (`live_verify`, `audit`, `release_diff`, `skip_live` with
  `--ignored`): all pass (14, 1, 1, 1).

## The first real build (y1-build, 2026-11-18)

Until this round nothing in the environment had compiled. The user allowed building one
recipe, `curl`, with whatever BitBake schedules for it, and cleaning and rebuilding curl
(only curl). The features written from the source alone (`pkgdata`, `sigs`, `log` on a
compile or configure failure, the `bitbake` tool's build jobs, `recipe show=cve`) were
checked against what that build produced. Raw outputs are in `{{LIVE}}/runs/y1-build/`;
trimmed copies in `tests/fixtures/outputs/live/y1-build/` (README there), checked in the
normal gate by `tests/live_build.rs`.

**Setup.** The kas build only (`{{LIVE}}/yocto-kiss/build`), entered with `env -u NETRC_FILE
../venv/bin/kas shell kas/kiss.yaml:kas/dogbonedark.yaml -c '<command>'` from `yocto-kiss/`,
or through `../scratch/kas-scratch.yaml` for the experiments (it adds
`scratch/meta-scratch`, where a `curl_%.bbappend` was put and removed again). sous-chef: the
release build of branch `y1-build`, `--config {{LIVE}}/runs/y1-build/sous-chef.toml`
(`build_dir`, `init_script = openembedded-core/oe-init-build-env`, `[bitbake] enabled =
true`, `allow_destructive = true`), `XDG_CACHE_HOME` and `TMPDIR` under `runs/y1-build/`, and
`PATH=/usr/sbin:/usr/bin:/sbin:/bin` (the `PATH` kas shell gives BitBake, so that the
init-script runner sees the same host tools); MCP calls by `runs/y1-build/tools/mcp.py`.

**The build.** `bitbake curl` from an empty `sstate-cache` (only curl's earlier fetch was
there): **succeeded** in **28 min 31 s** on the 16-core host, "Attempted 919 tasks of which 2
didn't need to be rerun and all succeeded", 78 recipes (`NOTE: recipe <PF>: task` lines: 65
native or cross, and 13 others: curl, its target dependencies openssl, zlib, libidn2,
libunistring, the toolchain's glibc, libgcc, libgcc-initial, gcc-runtime,
linux-libc-headers, gcc-source, opkg-utils, and ca-certificates in `all-oe-linux`). The build directory grew from
250 MB to 13 GB. Three warnings, none a failure: uninative disabled (host glibc 2.44 newer
than uninative's 2.43, as before), and two upstream fetch failures recovered from
`MIRRORS` (`ca-certificates` from deb.debian.org, `gnu-config-native` from
git.savannah.gnu.org). No host-tool problem on this Arch host.

### pkgdata: confirmed

After the build, every mode against `oe-pkgdata-util` in the same environment
(`y1-build/oe-pkgdata-util.out`):

| Call | sous-chef | oe-pkgdata-util |
|---|---|---|
| `find_path /usr/bin/curl` | `curl`, recipe curl, loc `FILES:curl meta:conf/bitbake.conf:338-347, meta:recipes-support/curl/curl_8.19.0.bb:202` | `curl: /usr/bin/curl` |
| `find_path *libcurl.so*` | 4 rows: `curl-dbg` `/usr/lib/.debug/libcurl.so.4.8.0`, `curl-dev` `/usr/lib/libcurl.so`, `libcurl (runtime name libcurl4)` `/usr/lib/libcurl.so.4` and `.so.4.8.0` | the same 4 (in `os.walk` order) |
| `list_pkg_files curl` / `libcurl` / `libcurl4` / `curl-dev` | 2 / 2 / 2 / 16 files with sizes; `libcurl4` noted as the runtime name of `libcurl` | the same files (`-r libcurl4` for the runtime name); sizes equal `list-pkg-files -l` |
| `lookup_recipe libcurl4`, `libcurl`, `curl-dev` | curl `8.19.0-r0` `meta:recipes-support/curl/curl_8.19.0.bb` | `curl` each |
| `list_pkgs curl` | the 8 `PACKAGES` with runtime name, packaged, size: `curl-staticdev` and `curl-locale` `no (empty)` | `list-pkgs -p curl`: the 6 packaged ones; `-r` gives `libcurl4` |
| `package_info libcurl` / `libcurl4` / `curl` | version `8.19.0-r0`, runtime name `libcurl4`, size 432452, `RDEPENDS` `glibc (>= 2.43+git0+562cd7badd) libcrypto (>= 3.5.9) libidn2 (>= 2.3.8) libssl (>= 3.5.9) zlib (>= 1.3.2)`, `RRECOMMENDS ca-certificates`, `RPROVIDES libcurl (=8.19.0)` | `libcurl 8.19.0-r0 curl 8.19.0-r0 432452`, `libcurl4 ...`, `curl 8.19.0-r0 curl 8.19.0-r0 156605` |

- **`PKGDATA_DIR`**: taken from the evaluated configuration (`note: PKGDATA_DIR = ... (from
  the evaluated configuration)`), equal to `bitbake-getvar --value PKGDATA_DIR`
  (`{{LIVE}}/yocto-kiss/build/tmp/pkgdata/dogbonedark`). The `bitbake-getvar` and search
  fallbacks were not needed here.
- **Renamed package**: `PKG:libcurl: libcurl4` (debian.bbclass), shown as `libcurl (runtime
  name libcurl4)`; found by its runtime name through `runtime-reverse/`.
- **Escapes**: one file name of the build has them, in ca-certificates
  (`NetLock_Arany_=Class_Gold=_Főtanúsítvány.crt`). sous-chef shows the real name (as on
  disk in `packages-split/`); `oe-pkgdata-util` prints `Főtanúsítvány`,
  as bitbake-semantics.md said.
- **Whole comparison** (`tests/live_build.rs`, `real_pkgdata_find_path_equals_oe_pkgdata_util`):
  `find_path *` over the recorded curl and ca-certificates package data gives the same 1062
  `(package, path)` pairs as `oe-pkgdata-util find-path '*'`; the sizes equal `list-pkg-files
  -l -p curl` and each `PKGSIZE` is their sum. Over the whole package data of the build
  (17 recipes) the ignored test `live_whole_pkgdata_find_path_equals_oe_pkgdata_util`
  compares all 10 505 pairs with the recorded `find-path '*'`: equal (run after the later
  rebuilds of curl too).

### sigs: confirmed, two fixes

Before any change, `sigs recipe=curl task=configure`: "only one signature: nothing to
compare", as `bitbake-diffsigs` (`ERROR: Only one matching sigdata file found ...`). Then
`scratch/meta-scratch/recipes-test/curl/curl_%.bbappend` with `EXTRA_OECONF:append = "
--disable-dict"`, entered through `kas-scratch.yaml` (a dry run `bitbake -n curl` with the
scratch layer and no bbappend first: "919 didn't need to be rerun", so adding the layer
changes no signature), and `bitbake curl`: 39.7 s, 11 tasks ran (`do_configure` to
`do_create_package_spdx`). The raw comparisons are in `y1-build/sigs/`.

| Task | `bitbake-diffsigs -t curl <task>` | sous-chef `sigs` (after the fixes) |
|---|---|---|
| `do_configure` | `basehash changed from 470219a9... to ba225ab2...`, `Variable EXTRA_OECONF value changed:` and a unified diff: the whole old and new value, one line each, then three `PACKAGECONFIG{...} = Set/Unset` context lines | `re-ran: its own inputs changed (basehash)`; `cause EXTRA_OECONF value changed: 1 line added, 1 removed; words added: --disable-dict` at `meta-scratch:recipes-test/curl/curl_%.bbappend:2` |
| `do_compile` | `Hash for task dependency curl:do_configure changed from 7ee05dbc... to 8a1a4041...`, then the configure comparison indented | `re-ran: ... by way of curl:do_configure`, `via curl:do_configure hash 7ee05dbc4c -> 8a1a4041e9`, the same cause `EXTRA_OECONF (curl)` with the same pointer |
| `do_install` | two levels | `by way of curl:do_compile -> curl:do_configure`, two `via` rows, the same cause |

Root cause, variable, pointer to the assignment in the scratch bbappend and the chain: all
right. Two things were wrong and are fixed (tests in `tests/live_build.rs` on the recorded
outputs):

- **The word added was not named, and the pointer was right by luck.** A value with a
  newline is printed as a unified diff whose `-` and `+` lines are the whole value (y3
  correction: `EXTRA_OECONF` does not start with one; the newlines are the `\n<VAR>{<word>}
  = Set|Unset` lines BitBake appends for the words its python tests,
  bitbake-semantics.md "Task signatures"); the digest said only "1 line added, 1 removed", and the
  "words the change added" used to find the assignment were every word of the `+` line, so
  any applied operation matched and the last one won. Now `bitbake::sigs::word_changes`
  takes the multiset difference of the `+` and `-` words (`--disable-dict`), the detail
  names it when lines were changed in place, and the pointer is the operation holding it.
- **BitBake client lines counted as unrecognised output.** `NOTE: Starting bitbake
  server...` (every run) and, when the previous `bitbake-diffsigs`'s server had not exited
  yet, `NOTE: Reconnecting to bitbake server...` / `NOTE: Retrying server connection (#1)...`
  (seven lines for the second call, 3 s after the first) gave "7 output lines were not
  recognised". `NOTE:` and `WARNING:` lines are skipped now.

Side observation: `--disable-dict` was already in `PACKAGECONFIG_CONFARGS` (curl's
`PACKAGECONFIG` does not enable dict), so the configure line had it twice and the output
did not change; the `do_packagedata` stamp kept its old hash (`db59de52...`), consistent
with hash equivalence giving `do_package` its old unihash. No change reached curl through
a dependency's hash in this round (nothing else changed); that case remains checked on
hand-written output only.

### log and the Bash hook on real failures: confirmed, three fixes

Two failures of curl made on purpose through the scratch bbappend, each with `bitbake curl`
through `kas-scratch.yaml` (exit 1 both times); raw consoles, task logs and run files in
`y1-build/log/`.

**A real `configure: error`** (`EXTRA_OECONF:append = " --with-libpsl"`; libpsl is not in
curl's `DEPENDS`): `log recipe=curl task=configure`, and `log` with no argument (which found
the same failure as the most recent one), 3.4 s and 0.2 s:

```
recipe	curl
version	8.19.0
task	do_configure
exit	1
error (configure, line 345)	configure: error: libpsl libs and/or directories were not found where specified!
context	checking for psl_builtin in -lpsl... no
function	oe_runconf at meta:classes-recipe/autotools.bbclass:96; called from autotools_do_configure < do_configure
definition	meta:classes-recipe/autotools.bbclass:149-225
log	build:tmp/work/armv7at2hf-neon-oe-linux-gnueabi/curl/8.19.0/temp/log.do_configure.1749535
handle	h:391e170707
```

Recipe, task, exit status (from the newest cooker console log), the first error with its
context, the failing function mapped through the run file (`autotools.bbclass:96` is the
`bbfatal_log "configure failed"` line: checked by hand from the `# line: 87` comment), the
task definition (`autotools_do_configure`, lines 149-225) and the handle; 574 bytes:
**confirmed**.

**A failing `do_compile:prepend`** (it echoes `lib/sous_chef_probe.c:12:5: error: 'probe'
undeclared (first use in this function)` and runs `false`): the first error and exit were
right, but

- **the function row said `do_compile at autogenerated:3`.** BitBake writes a `:prepend` of
  an `EXPORT_FUNCTIONS` stub inside the stub, under its `# line: 1, file: autogenerated`
  comment, so the mapping is BitBake's own and useless. Fixed: the tool looks up the
  applied operations of that function in the recipe's evaluation and points at the line
  equal to the trap's command: `do_compile at
  meta-scratch:recipes-test/curl/curl_%.bbappend:4 (run inside the EXPORT_FUNCTIONS stub)`.
  Test: `tests/bitbake_logs.rs`, `log_tool_points_a_failing_prepend_at_its_operation` (the
  real log and run file laid out in the fixture workspace).
- **the only context rows were the shell trap's** (`WARNING: <run file>:148 exit 1 from
  'false'`, `WARNING: Backtrace (BB generated script):`), which the function row already
  says. They are left out of the context now (`tests/live_build.rs`,
  `real_compile_failure_in_prepend_maps_to_autogenerated`).

**The Bash hook** on both consoles (a `PostToolUse` Bash event carrying the captured
output, `runs/y1-build/tools/hookrun.py`), with the command as it was really run, `env -u
NETRC_FILE ../venv/bin/kas shell ../scratch/kas-scratch.yaml -c 'bitbake curl'`:

- **it passed the output through** (2 ms, no response): `runs_bitbake` skipped `env` and
  the option `-u` but took `NETRC_FILE`, the option's value, for the command. This is the
  way docs/live-environment.md tells everyone to run kas here. Fixed: the options of `env`,
  `sudo`, `nice` and `time` that take a value skip it (`-u`, `--unset`, `-C`, `--chdir`,
  `-g`, `-n`, `-f`, `-o`, ...); unit tests in `src/hook/bash.rs`.
- after the fix: the 37 359-byte configure console is replaced by 583 bytes (recipe,
  version, task, exit 1, the `configure: error`, its context, `oe_runconf (run file line
  255)`, the definition, the handle and the slice hint), the 4 564-byte compile console by
  477 bytes; 18 to 21 ms per call. The hook did not read the run file, so its function row
  gave the run-file line, not the metadata line (the `log` tool did); since y3 it uses the
  `log` tool's mapping (below, "y3-leftovers").

Seen in the consoles (bitbake-semantics.md, "Build output", corrected): the `configure
failed` `ERROR:` (from `bbfatal_log`) did not stop knotty from printing `Log data follows:`
(it is logged with `forcelog`); in both consoles `Execution of '<run file>' failed with exit
code 1` came before `Logfile of failure stored in:`.

### The bitbake tool with a real build: confirmed, cancel fixed

Through the MCP server (init-script mode, `runs/y1-build/tools/calls-bitbake-*.json`), the
scratch bbappend removed first:

- `op=clean target=curl args=["cleanall"] confirm=true wait=300` with a progress token:
  job 1, `bitbake -c cleanall curl`, succeeded in 6 s; 14 progress notifications, about two
  a second, each `job:1 clean curl running <n>s: <last line>`.
- `op=build target=curl`: job 2 started at once (`started; poll with bitbake op=status
  job=job:2, stop with op=cancel`). `op=status` 5 s later: `running`, the last five lines.
- **Behind the build**, `op=env target=curl` and `op=layers` were refused at once with the
  running job and "already running (one job at a time): it holds BitBake's lock ...", and
  `var name=S recipe=curl authoritative=true` with "cannot run bitbake -e now: job:2 (build
  curl) is running ... use static mode"; `log job=job:2`: "still running". As documented.
- `op=status wait=300` with a progress token: 67 notifications (`job:2 build curl running
  15s: NOTE: recipe curl-8.19.0-r0: task do_configure: Started`, ...), then the finished
  job: `succeeded`, `elapsed 41s`, `exit 0`, the handle and the last three lines (`Attempted
  919 tasks of which 900 didn't need to be rerun and all succeeded.`).

**Bug: `op=cancel` did not stop the build.** A second run (cleanall, build, cancel 22 s in,
during `do_configure`, with `ps` sampled every second, `y1-build/bitbake/`): the job was
`cancelled` after 29 ms and its client was gone, but the BitBake server (`bitbake-server`,
in its own session, re-parented, as for `bitbake -e` before) and its workers were not in
the job's group: `do_configure` went on, `do_compile` started (`make -j 16`) 5 s after the
cancel, and the build ran to `do_create_package_spdx`; the cookerdaemon log has
`Disconnecting Client` at 23:16:35 and the build's end, then `No timeout, exiting.`, at
23:16:55. So a cancel only made the build invisible. Fixed (`Supervised::interrupt`,
`src/bitbake/env.rs`): a cancel sends SIGINT to the group, again 1 s later, and waits up to
60 s for the client to exit before terminating anything: what Ctrl-C twice does in knotty
(`stateShutdown`, `stateForceShutdown`). Re-run live with the fix: cancel answered after
3.6 s; the job output ends `Keyboard Interrupt, closing down...`, `Second Keyboard
Interrupt, stopping...`, `WARNING: Exiting due to interrupt.`, `NOTE: Sending SIGTERM to
remaining 1 tasks`, `Execution was interrupted, returning a non-zero exit code.`; the
daemon log has `stateShutdown` and `stateForceShutdown` 1 s apart and the server exited 2 s
later; no configure or make process after 23:24:06; `log.task_order` has nothing after that
`do_configure`; and an `op=layers` 3 s later ran at once (`succeeded`, the lock free). Test
with a fake client that behaves as knotty and ignores SIGTERM:
`tests/bitbake_jobs.rs`, `cancel_interrupts_the_client_twice_and_lets_it_stop`; the
recorded outputs in `tests/live_build.rs`, `real_cancelled_build_outputs`.

Not settled: `log job=` on a cancelled job says "no error lines recognised; slice the log"
(true; it could say the job was cancelled). Cancel with `BB_SERVER_TIMEOUT` set and a
kas-mode job (where SIGINT also reaches kas and the shell) were not tried. (Both but
`BB_SERVER_TIMEOUT` settled in y3, below.)

### recipe show=cve against a real report: confirmed

Which class can make a report for curl alone, read in OE-core Wrynose: `sbom-cve-check` /
`sbom-cve-check-recipe` need `python3-sbom-cve-check-native` and the CVE database recipes
(`sbom-cve-check-update-cvelist-native:do_patch`, `...-nvd-native:do_patch`, fetched lists),
so other recipes and the network: not used. `vex.bbclass` (in `classes/`, so inheritable
by a recipe) adds `do_generate_vex` (before `do_build`, no dependencies) that writes the
per-recipe JSON from the metadata and the patches alone. So the scratch bbappend became
`inherit vex`; `bitbake -n -c generate_vex curl` listed one task; `bitbake -c generate_vex
curl` ran it (succeeded) and wrote `tmp/deploy/cve/curl_cve.json` (11 266 bytes).

`recipe curl show=cve` (238 ms) then showed `build  Unpatched 0, Patched 21, Ignored 7
(version 8.19.0)  build:tmp/deploy/cve/curl_cve.json`, the identity (`CVE_PRODUCT` from
curl_8.19.0.bb:51, `CVE_VERSION` 8.19.0 from vex.bbclass:27), 7 `status` rows and 25
`patch` rows. Compared with the report (`runs/y1-build/tools/cvecompare.py`): the same 28
ids with the same status and detail (each `CVE_STATUS` entry `Ignored`, with its detail and
the report's description; each CVE of a patch file `Patched` / `fix-file-included`, four of
them from two patches: `CVE-2026-6429.patch` and `CVE-2026-6429-dependent.patch`, the
`-01` / `-02` pairs). **Confirmed**; test `tests/live_build.rs`, `real_vex_report_of_curl`
(the recorded report, the counts the view shows and the Wrynose file-name rule for all 25
patch files). The report has no NVD fields (no scores), as the class says. Side effect,
recorded in bitbake-semantics.md: the class's `BuildCompleted` handlers did not run (no
`cve-summary.json`; `tmp/log/cve/cve-summary-index.txt` was left in the build directory).

### Authoritative var and deps against the built curl: confirmed (known gaps flagged)

After the scratch bbappend was removed and curl rebuilt through yocto-kiss's own kas files,
`var recipe=curl` with and without `authoritative=true` (`runs/y1-build/tools/calls-var*.json`;
the first authoritative call ran `bitbake -e curl` in 3.9 s, the others reused its dump:
"reused: the tree is unchanged since it ran"):

| Variable | Static = authoritative? | Value |
|---|---|---|
| `SRC_URI` | **equal** (1174 bytes, the two handles compared byte for byte) | the tarball and 26 `file://` entries |
| `PACKAGECONFIG` | **equal** | `basic-auth bearer-auth digest-auth ipfs negotiate-auth openssl proxy threaded-resolver verbose zlib ipv6 aws libidn` |
| `S` | **equal** | `<WORKDIR>/sources/curl-8.19.0` |
| `B` | **equal** | `<WORKDIR>/build` |
| `D` | **equal** | `<WORKDIR>/image` |
| `WORKDIR` | **equal** | `{{LIVE}}/yocto-kiss/build/tmp/work/armv7at2hf-neon-oe-linux-gnueabi/curl/8.19.0` (the directory the build used) |
| `T` | **equal** | `<WORKDIR>/temp` |
| `PKGDATA_DIR` | **equal** | `<build>/tmp/pkgdata/dogbonedark` (the directory `pkgdata` read) |
| `EXTRA_OECONF` | differs, flagged ("1 value depends on python ... use authoritative=true") | static ends at `--disable-static`; BitBake's adds `PACKAGECONFIG_CONFARGS` (`--disable-ares --enable-aws ...`), which `base.bbclass`'s anonymous python sets (static: `""`) |
| `DEPENDS` | differs, flagged | static `pkgconf-native ${@get_autotools_dep(d)} ${@get_base_dep(d)}`; BitBake `pkgconf-native autoconf-native automake-native libtool-native libtool-cross gcc-cross-arm virtual/compilerlibs virtual/libc libidn2 openssl zlib virtual/update-alternatives` |

The two differences are the known ones (differential.md: the `PACKAGECONFIG` expansion and
two python helpers outside the evaluated set), and both are flagged.

**`deps curl` against what was built.** `kind=build`: `pkgconf-native`, the two unknown
python rows (`get_autotools_dep`, `get_base_dep`: in BitBake the autotools natives and
`libtool-cross`, and `virtual/cross-cc virtual/compilerlibs virtual/libc`), and from the
`PACKAGECONFIG` flags `libidn2`, `openssl` (`PREFERRED_PROVIDER_openssl`), `zlib`. The
build made exactly those target recipes plus what they need (`libunistring` for libidn2;
glibc, libgcc, gcc-runtime, linux-libc-headers for the toolchain), `opkg-utils` for
`virtual/update-alternatives` (added by `update-alternatives.bbclass`'s anonymous python
through `ua_extend_depends`: not in the static list, covered only by the generic warning
that this anonymous python "hands d to other code"), and `ca-certificates`. `kind=runtime`
lists `RDEPENDS` only (`curl-staticdev` -> `curl-dev`); the built packages' `RDEPENDS` in
the package data are the shared-library ones `do_package` adds (`libcurl`: glibc,
libcrypto, libidn2, libssl, zlib; `curl`: glibc, libcurl), which no static view can know,
and `RRECOMMENDS:libcurl: ca-certificates` (curl_8.19.0.bb:200), which is why
ca-certificates was built (no other built package names it). **Open**: `deps` has no
`RRECOMMENDS` rows, so the one recipe the build pulled in through them is not explained by
any `deps` view (`recipe show=packages` and `pkgdata package_info` show it).

### The build directory's layout: settled

From the build directory after these runs (bitbake-semantics.md, "Build output" and "Task
signatures", where each is recorded): `WORKDIR` = `tmp/work/<MULTIMACH_TARGET_SYS>/<PN>/<PV>`
with the subdirectories a full build makes; `T` = `WORKDIR/temp` with
`log.do_<task>.<pid>`, `run.do_<task>.<pid>` and `depsig.do_<task>.<pid>` (each with its
symlink) and `run.<helper>.<pid>`; `log.task_order` appended per run until `WORKDIR` is
removed (format checked on 31 real lines, `real_log_task_order_format`); the run files'
`# line: N, file: F` comments and the mapping the digester applies (checked on a real
`oe_runconf` frame, `real_configure_failure_log_and_line_mapping`), with
`EXPORT_FUNCTIONS` stubs as `# line: 1, file: autogenerated`; stamps
`<PV>.<task>.<hash>` and `<PV>.<task>.sigdata.<hash>` (the `do_packagedata` stamp with the
`.dogbonedark` suffix, none for `do_build`'s sigdata) and the sstate cache names with
`.siginfo`. Not settled: a second BitBake client's failure behind a long build (only one
client at a time was allowed here), cancel with `BB_SERVER_TIMEOUT` set.

### BitBake commands run in y1-build

All in the kas build, one client at a time; curl the only target, the only recipe cleaned.

| Command | Through | Result |
|---|---|---|
| `bitbake curl` | kas, yocto-kiss files | succeeded, 919 tasks, 28 min 31 s (the first build) |
| `bitbake-getvar --value PKGDATA_DIR`, `oe-pkgdata-util find-path / list-pkg-files / lookup-recipe / list-pkgs / package-info` (three shells) | kas, yocto-kiss files | read-only |
| `bitbake-diffsigs -c never -t curl do_configure` (sous-chef `sigs`, before the change) | init-script runner | "Only one matching sigdata file" |
| `bitbake -n curl` | kas, `kas-scratch.yaml`, no bbappend | nothing to run |
| `bitbake curl` with `EXTRA_OECONF:append = " --disable-dict"` | kas-scratch | succeeded, 11 tasks |
| `bitbake-diffsigs -c never -t curl do_configure / do_compile / do_install` | kas-scratch, and through sous-chef's `sigs` (init-script) twice | the comparisons above |
| `bitbake curl` with `--with-libpsl` | kas-scratch | failed in `do_configure` (on purpose) |
| `bitbake curl` with the failing `do_compile:prepend` | kas-scratch | failed in `do_compile` (on purpose) |
| `bitbake -c cleanall curl`, then `bitbake curl` (`bitbake` tool) | init-script | succeeded, 6 s and 41 s |
| `bitbake -c cleanall curl`, `bitbake curl`, cancel (twice: before and after the fix); `bitbake-layers show-layers` after the second | init-script | cancelled; the first build went on to its end. An `op=layers` was also started 3 s after that first cancel and was killed when the MCP client closed a moment later: unintended, a second client while the orphaned build still ran (the cookerdaemon log shows no connection from it, and the build finished normally) |
| `bitbake -e curl` (authoritative `var`) | init-script | read-only, 3.9 s |
| `bitbake -n -c generate_vex curl`, `bitbake -c generate_vex curl` with `inherit vex` | kas-scratch | one task, succeeded |
| `bitbake curl` (bbappend removed) | kas, yocto-kiss files | succeeded, 11 tasks ran, `bblayers.conf` and `local.conf` back as before |

## The leftovers of the first build (y3-leftovers, 2026-11-18/19)

The items y1-build left, on the same environment, under the same rule (curl the only
target and the only recipe cleaned, one BitBake client at a time, experiments in a
`curl_%.bbappend` in `scratch/meta-scratch`). sous-chef: the release build of branch
`y3-leftovers`, configurations in `{{LIVE}}/runs/y3-leftovers/` (`sous-chef.toml`: the
init-script mode of y1; `sous-chef-kas.toml`: **kas mode**, yocto-kiss's own kas files,
no `init_script`, `venv/bin` on the server's `PATH`), `XDG_CACHE_HOME` and `TMPDIR` under
`runs/y3-leftovers/`; MCP calls by `runs/y3-leftovers/tools/mcp.py`, hook calls by
`tools/hookrun.py`; raw outputs in `runs/y3-leftovers/logs/`, recorded copies in
`tests/fixtures/outputs/live/y3-leftovers/` (README there). `build/conf/bblayers.conf` and
`local.conf` were copied before (`conf-before/`) and after (`conf-after/`): identical.

### deps and what the build built: RRECOMMENDS, task depends, native variants

`bitbake -g curl` (kas shell, yocto-kiss files; `task-depends.dot` and `pn-buildlist`
moved to `runs/y3-leftovers/graph/`): 78 recipes, the 78 of the first build. Read against
it and the BitBake / OE-core source (bitbake-semantics.md, "Runtime dependencies and
recommendations", new): BitBake builds for `RDEPENDS` and `RRECOMMENDS` alike (`bb.cache`
`rundeps` / `runrecs`, `bb.taskdata`); `RSUGGESTS` and the other runtime variables cause no
task. `ca-certificates` is in the graph only as `curl.do_build -> ca-certificates.do_package_write_ipk`
/ `do_package_qa` and `curl.do_package_write_ipk -> ca-certificates.do_packagedata`
(`rdeptask`), and no relation of curl but `RRECOMMENDS:libcurl` names it.

Changed: `deps kind=runtime` has a `relation` column (`depends` / `recommends`) and lists
`RRECOMMENDS:<pkg>`, the plain `RDEPENDS` / `RRECOMMENDS` (every package) and
`PACKAGECONFIG` field 5; on curl: `libcurl ca-certificates recommends
RRECOMMENDS:libcurl meta:recipes-support/curl/curl_8.19.0.bb:200`, `curl-dev curl
recommends RRECOMMENDS:curl-dev meta:conf/bitbake.conf:364`, `curl-staticdev curl-dev
depends`. `kind=build` lists the recipes the tasks' `depends` flags name (`quilt-native`,
`patch-replacement-native` from `do_patch[depends]`, `binutils-cross-arm` from
`do_populate_sysroot[depends]` after the virtual-provider rewrite, the python words of
`do_package_write_ipk[depends]` and `do_create_spdx[depends]` as unknown rows) and reads a
native variant's `DEPENDS` through `native_filter` as BitBake does: before, `python3-native`
at depth 2 listed `expat`, `libffi`, `bzip2` with the **target** recipes as providers;
now `expat-native`, ... (source `DEPENDS (native_filter)`). Tests:
`tests/deps_runtime_tasks.rs`.

`deps curl depth=5` plus `kind=runtime` compared with `pn-buildlist`
(`runs/y3-leftovers/tools/depscompare.py`, output `logs/depscompare.out`): **43 of the 77
other recipes are named by a row** (32 before the native filter and the task rows). The 34
not named are each reached in the graph, by inspection of their parents, only through
what the output says it does not know:

- curl's `${@get_autotools_dep(...)}` and `${@get_base_dep(...)}` rows (unknown: python):
  `autoconf-native`, `automake-native`, `libtool-native`, `libtool-cross`, `gcc-runtime`,
  `libgcc`, `gnu-config-native`, and below them `m4-native`, `gmp-native`, `mpfr-native`,
  `libmpc-native`; `gettext-native` and `gettext-minimal-native` under the
  `${@gettext_dependencies(...)}` rows of libidn2 and others; `gawk-native` under glibc's
  `gawk-replacement-native` (provider `-`: a `PROVIDES:append:class-native`, which provider
  lookup does not find, as the `-` note says);
- anonymous python (the warnings name the classes, the task-depends note names what they
  add): `pseudo-native` (`virtual/fakeroot-native`, base.bbclass), `rpm-native`,
  `dwarfsrcfiles-native` (`PACKAGE_DEPENDS`, package.bbclass), `opkg-utils`
  (update-alternatives.bbclass), `opkg-utils-native` (the python word of
  `do_package_write_ipk[depends]`), `ninja-native` (cmake.bbclass, for expat-native), and
  what those need: `file-native`, `libarchive-native`, `libgcrypt-native`,
  `libgpg-error-native`, `lua-native`, `elfutils-native`, `libcap-native`,
  `e2fsprogs-native`, `util-linux-native`, `libcap-ng-native`, `libpcre2-native`,
  `lzlib-native`, `re2c-native`;
- the build dependencies of a runtime dependency (the runtime note says they are built
  too): `debianutils-native` for ca-certificates.

Still not shown statically: `binutils-cross-arm` and `gcc-cross-arm` are listed with
provider `-` (their `PN` is `binutils-cross-${TARGET_ARCH}`; the note now says a name that
needs expansion is not found).

### A real compiler error: confirmed, one fix

A patch added to curl's `SRC_URI` from the scratch bbappend turns one line of
`lib/version.c` into a type error (`y3-leftovers/break-compile.patch`); `bitbake curl`
through `kas-scratch.yaml` failed in `do_compile` (exit 1) with gcc's
`../../sources/curl-8.19.0/lib/version.c:152:25: error: incompatible types when
initializing type 'int' using type 'struct curl_slist'`, after `In function
'curl_version':` and before the source line and caret; then the `make[2]: ***` lines,
`ERROR: oe_runmake failed` and the trap (`bbfatal_log` < `die` < `oe_runmake` <
`autotools_do_compile` < `do_compile`). `log recipe=curl task=compile` (4.2 s, with
`bitbake-getvar`), `log` (0.5 s) and the Bash hook on the 378 136-byte console (27-33 ms):
recipe, version, task `do_compile`, exit 1, the first error, the three context lines (the
compiler's own), no `more` row (the make and `oe_runmake` lines are consequences; there was
one distinct error), `function autotools_do_compile at meta:classes-recipe/autotools.bbclass:228`
(the `oe_runmake` line, through the run file, in the hook too), the definition
`autotools.bbclass:227-229`, 732 bytes. **Wrong:** the error row was cut to the snippet
width, before the type (`... using type 'st...`), because the compiler's path takes 50
characters. Fixed: a compiler diagnostic shows the message in the `error` row and the
location in a `source` row (`source ../../sources/curl-8.19.0/lib/version.c:152:25`); JSON
keeps the whole line. Test: `tests/live_build.rs`, `real_compiler_error`, on the trimmed
log, run file and console.

### log job= on a cancelled job, and cancel in kas mode: settled, one fix in kas mode

**kas mode is used here**: `status` with `sous-chef-kas.toml` gave `bitbake build
environment found (kas)`; yocto-kiss's kas files include no other repository's file (the
reason kas mode is turned off, bitbake-semantics.md "kas"; only the scratch file
`kas-scratch.yaml` has `{repo, file}` includes). kas 5.5, read: `plugins/shell.py` runs
`subprocess.call([$SHELL, "-c", cmd])` and stays the parent; `kas.py` registers SIGINT
and SIGTERM on its asyncio loop, whose handler only wakes the loop, and the loop is not
running during that call, so kas neither exits nor kills its child on SIGINT
(bitbake-semantics.md, "Running BitBake").

**Bug found first: the server's `TMPDIR` became BitBake's.** The first kas-mode run
(`op=clean args=["cleanall"]`, `op=build`, `op=cancel` 22 s in) ran curl's `cleanall` and
build in `{{LIVE}}/runs/y3-leftovers/tmp`, the `TMPDIR` the server was started with: kas
copies `TMPDIR` (and six other variables) into the build environment and adds them to
`BB_ENV_PASSTHROUGH_ADDITIONS` (`libkas.py` `get_build_environ`). 721 MB was restored from
sstate there (`saved_tmpdir` named it), and `log job=` found no log in the build directory.
That directory was deleted afterwards (its cooker logs kept in `logs/leaked-tmpdir/`);
`build/tmp` was not touched by it (the `cleanall` there removed curl's sstate objects and
download stamp in the shared `sstate-cache` / `downloads`, which a `cleanall` of curl does
anyway). Fixed: kas mode removes `TMPDIR` from kas's environment (`KAS_DROPPED_ENV`); test
`tests/bitbake_kas.rs`, `kas_mode_does_not_hand_the_servers_tmpdir_to_kas`. The init-script
mode is not affected (OE-core's passthrough list has no `TMPDIR`; y1 built in `build/tmp`
with the same `TMPDIR` set).

*Superseded by z1-env:* `KAS_DROPPED_ENV` became `STRIPPED_ENV`, applied in every runner
mode (init-script, bitbake-setup, kas, initialised), because the init-script and initialised
modes import `TMPDIR` too when a passthrough list names it (a kas shell's does), with
`[bitbake] pass_env` to pass it on purpose and a `status` row when it is withheld. The rule,
the collision table and the live check of three modes are in docs/bitbake-semantics.md,
"The environment BitBake takes from its caller".

**Cancel through `kas shell`, after the fix** (`tools/calls-kas-cancel.json`, `ps` every
second in `logs/ps-kas-cancel2.txt`): `op=clean args=["cleanall"]` succeeded in 6 s;
`op=build` started at once (its client printed `NOTE: Reconnecting to bitbake server...`:
the `cleanall`'s server, its client gone, was still exiting); `op=cancel` 22 s in, during
`do_configure`, answered after 3.4 s; the job group was kas's (`kas` pid = pgid, `bitbake`
its child: bash had `exec`ed it). The output ends as in init-script mode, `Keyboard
Interrupt, closing down...`, `Second Keyboard Interrupt, stopping...`, `WARNING: Exiting
due to interrupt.`, `NOTE: Sending SIGTERM to remaining 1 tasks`, `Execution was
interrupted, returning a non-zero exit code.`, then kas's `Shell returned non-zero exit
status`; no configure process after 23:53:09 (the cancel was sent at 23:53:08), and no
BitBake process in the samples from 23:53:12, when the cancel answered, until the next job
30 s later. Test with a fake kas that stays the parent and defers both signals as kas does:
`tests/bitbake_kas.rs`, `a_cancel_reaches_bitbake_through_kas_shell`.

**`log job=job:2`** on that job (6 ms) now says what happened instead of "no error lines
recognised":

```
job	job:2 (build curl)
command	bitbake curl
state	cancelled by the bitbake tool (op=cancel) at 2026-11-18 21:53:12 UTC, 25s after it started: not a failure
bitbake	Keyboard Interrupt, closing down... / Second Keyboard Interrupt, stopping... / NOTE: Sending SIGTERM to remaining 1 t...
running	curl-8.19.0-r0 do_configure (task 909 of 919)
log	build:tmp/work/armv7at2hf-neon-oe-linux-gnueabi/curl/8.19.0/temp/log.do_configure.1910658
warning	WARNING: Your host glibc version (2.44) is newer than that in uninative (2.43). Disabling uninative so that sstate is...
warning	WARNING: Exiting due to interrupt.
handle	h:48854485ad
note: that log is the interrupted do_configure of curl-8.19.0-r0, written up to the cancel; log recipe=curl task=configure digests it
```

The interrupted task's log just stops (`checking if siginterrupt is prototyped...`):
nothing in it records the interrupt. Test on the recorded job output, replayed by the fake
BitBake around the two interrupts: `tests/bitbake_jobs.rs`,
`log_job_on_a_cancelled_job_says_what_happened`; the parsing on all three recorded cancels
(y1 SIGINT, y1 SIGTERM, y3 kas): `tests/live_build.rs`,
`real_cancelled_builds_name_the_running_task_and_the_interrupt`.

### A finished job's warnings: fixed

`op=build target=curl wait=300` in kas mode (39 s, `Attempted 919 tasks of which 908 didn't
need to be rerun`): the report now has `warning WARNING: Your host glibc version (2.44) is
newer than that in uninative (2.43). ...` and a tail without `Summary: There was 1 WARNING
message.`; the cancelled job lists its two warnings the same way. The `WARNING:` lines are
de-duplicated (repeats counted), at most five shown, then `+N more distinct warnings` with a
`slice ... grep=^WARNING:` hint; the Bash hook lists them the same way above its tail.
Tests on the first build's console with its three warnings (trimmed): `tests/bitbake_jobs.rs`,
`a_finished_job_lists_its_warnings_not_their_count`, `tests/hook_bash.rs`,
`the_hook_lists_the_warnings_of_a_real_successful_build`, `tests/live_build.rs`,
`real_build_warnings`.

### The hook's function row: the log tool's mapping

The Bash hook now reads the run file the trap names (next to the task log the console
names, confined) and calls the `log` tool's own functions (`read_run_file`,
`place_stub_function`). On the real compiler-error console above: `function
autotools_do_compile at meta:classes-recipe/autotools.bbclass:228` (was `(run file line
153)`). On y1's failing `do_compile:prepend` console, laid out under `runs/y3-leftovers/hooktest/`
with the prepend in a test layer there (a copy of the build directory's paths only, never
parsed by BitBake): `do_compile at meta-hooktest:recipes-test/curl/curl_%.bbappend:4 (run
inside the EXPORT_FUNCTIONS stub)`, **64-68 ms** per call with the recipe evaluated, under
the hook's 150 ms budget; the evaluation is skipped (the row then names the stub) when less
than 80 ms is left. Tests on the recorded y1 consoles and run files: `tests/hook_bash.rs`,
`the_hook_maps_a_real_failing_frame_to_its_metadata_line`,
`the_hook_points_a_real_failing_prepend_at_its_operation`.

### bitbake-semantics.md, the y1 edits reviewed

Each changed paragraph was read against its evidence (the recorded outputs, the BitBake
2.18.0 source, the build directory). Corrected: the unified diff of `EXTRA_OECONF` is not
because "the value starts with a newline" (it does not) but because BitBake appends a
`\n<VAR>{<word>} = Set|Unset` line per word of its `bb.utils.filter` (`data.py`
`handle_contains`) and `siggen` diffs any value with a newline (the fourth word,
`schannel`, is outside difflib's three context lines); a `forcelog` `ERROR` does not just
"not count", it clears the flag (`ErrorCheckHandler.emit`), so the log is printed whenever
the task's last `ERROR` is a `forcelog` one; the VEX issues do have an NVD field, `link`.
Added from the build directory: `_setscene` logs and run files, `sstate-install-<task>/`,
and `.siginfo` files for every task run, not only the 11 with archives; the source of
`Sending SIGTERM to remaining N tasks` (`bin/bitbake-worker`). Confirmed as written: the
VEX file contents and handlers, the `log.task_order` format and its restart after
`cleanall` (seen again), the 21 / 11 counts of y1's `T`, the trap lines and the
`autotools.bbclass:96` mapping, the stub layout, the `configure:` NOTE lines and the 355
`| ` lines, the diffsigs indentation and client lines, the stamp names, the package data
details (`PKGSIZE`, the two-space `RPROVIDES`, `:\s+` in `read_pkgdatafile`, empty
packages, `extended/` and `debugsources/`), and knotty's interrupt handling.

### BitBake commands run in y3-leftovers

All in the kas build directory, one BitBake client at a time (`tools/bbcheck.sh` before
each; a server still exiting after its client was seen twice, never two clients); curl the
only target, the only recipe cleaned.

| Command | Through | Result |
|---|---|---|
| `bitbake -c cleanall curl`, `bitbake curl`, cancel | sous-chef kas mode, before the TMPDIR fix | ran in the server's `TMPDIR` (`runs/y3-leftovers/tmp`, deleted afterwards); cancelled in 3.5 s |
| `bitbake -c cleanall curl`, `bitbake curl`, cancel | sous-chef kas mode | cleanall 6 s; cancelled 25 s in (`do_configure`), 3.4 s |
| `bitbake curl` | sous-chef kas mode | succeeded, 39 s, 908 of 919 not rerun |
| `bitbake -g curl` | kas, yocto-kiss files | dependency graph only; its two files moved out of `build/` |
| `bitbake curl` with the `break-compile.patch` bbappend | kas, `kas-scratch.yaml` | failed in `do_compile` (on purpose) |
| `bitbake-getvar -r curl --value T`, twice (`log recipe=curl task=compile`) | sous-chef init-script mode | read-only |
| `bitbake curl` (bbappend removed) | kas, yocto-kiss files | succeeded, 918 of 919 not rerun (curl from sstate) |

## Three features never run for real (real-checks, 2026-11-20)

`sigs` through a dependency's hash, multiconfig against a real BitBake, and the Bash and
Edit hooks in a live Claude Code session. sous-chef: the release build of branch
`real-checks`; configurations, cache (`XDG_CACHE_HOME`), `TMPDIR`, helper scripts
(`tools/`) and raw outputs in `{{LIVE}}/runs/real-checks/`; recorded copies in
`tests/fixtures/outputs/live/real-checks/` (README there).

### sigs through a dependency's hash: confirmed, two fixes

The user allowed building `curl` and `zlib` for this item. In the kas build, one BitBake
client at a time (`tools/bbcheck.sh` before each), `build/conf/bblayers.conf` and
`local.conf` copied before (`conf-before/`) and after (`conf-after/`): identical.

**The change.** `scratch/meta-scratch/recipes-test/zlib/zlib_%.bbappend` through
`kas-scratch.yaml`. The first version, `CFLAGS:append = " -O1"`, was dropped after its dry
run: the bbappend applies to `zlib-native` too, a dependency of most native recipes, and
`bitbake -n curl` wanted 83 sstate objects and found none (glibc, openssl, python3-native,
... would have rebuilt). That dry run was not free: it "removed 83 stale sstate objects"
from `build/tmp` (bitbake-semantics.md "Task signatures": `StaleSetSceneTasks` fires in a
dry run too); the next build restored them from the sstate cache. The second,
`CFLAGS:append:class-target = " -O1"` (`-O1` changes the object code, so zlib's output
hash and with it curl's dependency hashes change despite hash equivalence): dry run 899
of 919 not rerun, only zlib (8) and curl (12) tasks. `bitbake curl`: 76 s, succeeded;
the tasks that ran (not `_setscene`) were zlib's `do_configure` to `do_packagedata` and
curl's `do_prepare_recipe_sysroot` to `do_package_write_ipk`; the 71 native objects the
first dry run had removed came back as `_setscene`.

**BitBake against sous-chef** (`bitbake-diffsigs -c never -t <pn> <task>` through
`kas-scratch.yaml`; `sigs` through the init-script runner, the scratch layer in
`bblayers.conf` at the time):

| Task | `bitbake-diffsigs` | sous-chef `sigs` |
|---|---|---|
| `curl do_configure` | 5 nested `Hash for task dependency` lines: `curl:do_prepare_recipe_sysroot`, `zlib:do_populate_sysroot`, `zlib:do_install`, `zlib:do_compile`, `zlib:do_configure`; then `basehash changed` and `Variable CFLAGS value changed:` `"${TARGET_CFLAGS} -D_REENTRANT -fPIE {+-O1+}"` | the same five `via` rows with the same hashes, `cause CFLAGS (zlib)` with the word diff at `meta-scratch:recipes-test/zlib/zlib_%.bbappend:3` (the `:append:class-target` line); 8.9 s |
| `curl do_compile` / `do_install` | one / two more levels (`curl:do_configure`, `curl:do_compile`) | the same chain, the same cause and pointer |
| `zlib do_configure` | `basehash changed`, the `CFLAGS` word diff | `its own inputs changed (basehash)`, the same cause and pointer |
| `zlib do_compile` | only `Hash for task dependency zlib:do_configure changed` and that comparison | (before the fix) `its own inputs did not change; a dependency's did` |

Chain, root cause, the owner recipe (zlib, from the innermost dependency) and the pointer
to the assignment in the bbappend: **right**. Two things were wrong, both fixed in
`src/mcp/tools/sigs.rs`:

- **"its own inputs did not change" was a guess.** zlib's `do_compile`, `do_install` and
  `do_populate_sysroot` each had their own basehash changed (the two sigdata files of each
  task decoded with Python's `compression.zstd`, `tools/sigbase.py`:
  `sigdata-basehashes.out`; `CFLAGS` is in each task's `varvals`), but `bitbake-diffsigs`
  printed only the dependency: `compare_sigfiles` replaces the task's own lines by the
  dependency's comparison (bitbake-semantics.md "Task signatures"). The row now says
  `re-ran: a dependency's hash changed, by way of ...` and a note says a change of the
  task's own inputs would not be listed. (For curl's tasks the old claim happened to be
  true: their basehashes were unchanged.)
- **The cause was cut by the budget on a deep chain.** `sigs curl install` (seven
  dependencies) at the default 250 tokens showed the seven `via` rows and dropped the one
  `cause` row (`+2 more over budget_tokens`). The `via` rows now come after the causes.

Tests: `tests/live_build.rs` `real_diffsigs_through_a_dependency_hash`,
`real_diffsigs_hide_a_tasks_own_change_behind_a_dependency` (the recorded outputs and
sigdata summary); `tests/sigs.rs`
`real_change_through_a_dependency_points_at_the_dependencys_bbappend` (the real
`do_configure` and `do_install` outputs replayed through the tool on a fixture copy with a
zlib recipe and the same bbappend: chain, pointer, wording, cause before the path at the
default budget); the two hand-written snapshots updated for the wording and order.

**Restored.** The bbappend removed, `bitbake curl` through yocto-kiss's own kas files:
`Sstate summary: Wanted 40 Local 40 ... 100% match`, "918 of 919 didn't need to be
rerun" (no task but `_setscene` ones; two "Removing N stale sstate objects" lines for the
`-O1` outputs), then `bitbake -n curl`: "919 didn't need to be rerun". `bblayers.conf`
and `local.conf` byte-identical to `conf-before/`. `scratch/meta-scratch/recipes-test/`
holds only `broken-fetch`.

BitBake commands of this item, all in `{{LIVE}}/yocto-kiss/build`, one client at a time:

| Command | Through | Result |
|---|---|---|
| `bitbake -n curl` with `CFLAGS:append = " -O1"` | kas-scratch | dry run; 83 sstate misses; removed 83 stale objects from `tmp` |
| `bitbake -n curl` with `CFLAGS:append:class-target = " -O1"` | kas-scratch | dry run; only zlib and curl tasks |
| `bitbake curl` with that bbappend | kas-scratch | succeeded, 76 s; zlib's and curl's tasks ran, the rest from sstate |
| `bitbake-diffsigs -c never -t curl do_configure / curl do_compile / zlib do_configure / zlib do_compile` | kas-scratch | the comparisons above |
| `bitbake-diffsigs -c never -t ...` (sous-chef `sigs`: curl configure, compile, install; zlib configure, compile; three runs of the server) | sous-chef init-script mode | read-only |
| `bitbake curl` (bbappend removed) | kas, yocto-kiss files | succeeded, 918 of 919 not rerun |
| `bitbake -n curl` | kas, yocto-kiss files | 919 not rerun |

### Multiconfig against a real BitBake (Scarthgap, parse only): confirmed, one BitBake quirk

In the Scarthgap release workspace (`{{LIVE}}/releases/scarthgap/build`, BitBake 2.8.1),
its `conf/` copied first (`runs/real-checks/scarthgap-conf-before/`): `BBMULTICONFIG =
"musl"` and an `mcdepends` on curl alone appended to `local.conf`
(`MCDEP_REALCHECKS:pn-curl = "mc::musl:zlib:do_populate_sysroot"`, `do_install[mcdepends]
= "${MCDEP_REALCHECKS}"`), and `conf/multiconfig/musl.conf` with `TCLIBC = "musl"` and
`TMPDIR = "${TOPDIR}/tmp-musl"` (recorded in
`tests/fixtures/outputs/live/real-checks/multiconfig/`). Afterwards the file and directory
were removed and `local.conf` copied back: `diff -r` against the copy is empty, the five
files' sha256 as before. BitBake left `build/tmp-musl/` (904 KB).

**The differential with a multiconfig.** `tests/differential.rs` takes a recipe written
`mc:<name>:<recipe>` (BitBake side: `bitbake -e mc:<name>:<recipe>` through
`scripts/differential.sh`, unchanged; static side: `Target::mc`, provider selection with
`Providers::new_in(.., mc)`), extra variables per workspace (`vars =`) and files a run
needs (`requires =`, so `differential_scarthgap_mc` returns at once now that the
multiconfig is gone). Workspace `tests/fixtures/differential/scarthgap-mc.ws`: zlib, curl,
busybox, quilt-native, core-image-minimal and virtual/libc in both configurations (12
`bitbake -e`, 5.8 to 78 s, the first parsing both configurations), plus `BB_CURRENT_MC`,
`TCLIBC`, `TCLIBCAPPEND`, `TARGET_SYS`, `MULTIMACH_TARGET_SYS`,
`PREFERRED_PROVIDER_virtual/libc`, `STAGING_DIR_TARGET`, `SSTATE_DIR`, `DEPLOY_DIR`.
`show-layers.out` and `show-appends.out` were copied from the acc-releases run (same
layers) so that no `bitbake-layers` ran. Result
(`tests/fixtures/outputs/live/differential-scarthgap-mc/`): **659 values, 600 equal, 46
flagged, 2 `gap:python-at-parse`, 11 `gap:variant-layername`, 0 bugs**. Every variable the
second configuration changes is equal in all 12 targets (the two `flagged` are
quilt-native's `TARGET_SYS` / `MULTIMACH_TARGET_SYS`, flagged in both configurations); in
the musl configuration every other difference is flagged or the same one its default
counterpart has (busybox's `DEPENDS`, `ua_extend_depends`), except the 11 below.

**BitBake's `-e` of a multiconfig recipe has `FILE_LAYERNAME` `None`** (and so
`layer-${FILE_LAYERNAME}` in `OVERRIDES`) in all six musl dumps; the static side says
`core`. `bitbake-getvar -r mc:musl:curl FILE_LAYERNAME` (a real parse) prints `core`: the
`-e` path looks the layer up by the virtual file name `mc:musl:<file>`, as it does for a
`virtual:native:` variant (bitbake-semantics.md "Multiconfig"). The harness's
`gap:variant-layername` now covers both (`Ctx::in_mc`); nothing to fix in sous-chef. Also
seen: the include history of `bitbake -e mc:musl:zlib` is the default configuration's
(`multiconfig/default.conf`, not `musl.conf`).

**The tools** (`sous-chef-scarthgap.toml`, static; `tools/calls-mc.json`) against
`bitbake-getvar` (`tools/getvar-scarthgap.sh`):

| Question | sous-chef | BitBake |
|---|---|---|
| `status` | `multiconfigs BBMULTICONFIG = "musl" (build:conf/local.conf:11)`, `multiconfig musl build:conf/multiconfig/musl.conf:1`, `multiconfig_present default meta:conf/multiconfig/default.conf:1 (not in BBMULTICONFIG)` | `bitbake-getvar --value BBMULTICONFIG`: `musl` |
| `var TCLIBC recipe=curl mc=musl` and `recipe=mc:musl:curl` | `shadowed ??= bitbake.conf:394`, `applied = build:conf/multiconfig/musl.conf:2`, `final: "musl"`, the multiconfig note | `musl`; the dump's history the same two operations |
| `var TCLIBC recipe=curl` | `glibc` | `glibc` (dump) |
| `var WORKDIR recipe=mc:musl:zlib` | `<build>/tmp-musl/work/core2-64-poky-linux-musl/zlib/1.3.1` | the same |
| `var FILE_LAYERNAME recipe=mc:musl:curl` | `core` | `core` (getvar; the `-e` dump: `None`, above) |
| `var PREFERRED_PROVIDER_virtual/libc mc=musl` | `musl`, `tclibc-musl.inc:9` | `musl` |
| `deps virtual/libc kind=providers` with and without `mc=musl` | `musl` selected / `glibc` selected | `bitbake -e mc:musl:virtual/libc` parsed `musl_git.bb`, `bitbake -e virtual/libc` `glibc_2.39.bb` |
| `var do_install[mcdepends] recipe=curl` | `final[mcdepends]: "mc::musl:zlib:do_populate_sysroot"` from `build:conf/local.conf:14` | `bitbake-getvar --value -f mcdepends -r curl do_install`: the same (`-u`: `${MCDEP_REALCHECKS}`) |
| `deps curl kind=mcdepends` | `do_install (default) musl zlib do_populate_sysroot applies here build:conf/local.conf:14` | the flag above |
| `deps mc:musl:curl kind=mcdepends` | the same row, `not in this configuration (BitBake adds it only when building in (default))` | `-r mc:musl:curl`: the flag is set there too; `add_mcdepends` adds it only `if mc == frommc` (source) |
| `deps zlib kind=mcdepends` | `no task of this recipe has an mcdepends flag` | `-r zlib`: the flag is set, expanding to `""` |
| `var nope mc=nope` | error `multiconfig nope; known: musl` | (not run) |

All agree. Two small things not fixed: `deps zlib kind=mcdepends` says no task "has an
mcdepends flag" where BitBake has the flag with an empty value (no entry either way); and
the `kind=mcdepends` answers end with `none changes DEPENDS by name` (the anonymous-python
note names `DEPENDS` for every kind but runtime and packageconfig, `deps_var` in
`src/mcp/tools/deps.rs`). Not run: a `bitbake -g` that would show the mc dependency in the
task graph (only `-e` and `bitbake-getvar` were allowed). Test on the recorded files:
`tests/differential.rs` `recorded_scarthgap_multiconfig` (normal gate).

BitBake commands of this item, all in `{{LIVE}}/releases/scarthgap/build`, one client at
a time, parse only: `bitbake -e` of the 12 targets (`scripts/differential.sh`,
`SOUS_CHEF_DIFF_WS=scarthgap-mc`), and ten `bitbake-getvar` (`--value BBMULTICONFIG`; `-f
mcdepends` for curl, `-u` for curl, zlib, `mc:musl:curl`; `TCLIBC` and `FILE_LAYERNAME` for
`mc:musl:curl`, `FILE_LAYERNAME` for curl; `PREFERRED_PROVIDER_virtual/libc` and `WORKDIR`
for `mc:musl:zlib`).

## Left in the environment

`{{LIVE}}/scratch/` (all created by T9.2): `sous-chef.toml`, `sous-chef-big.toml`,
`sous-chef-huge.toml`, `sous-chef-setup.toml`; `cache/` (the indexes and handles,
`XDG_CACHE_HOME`); `meta-scratch/` (the scratch layer with `broken-fetch_1.0.bb`);
`kas-scratch.yaml`; `wrapper-test/` (the bug B7 reproduction scripts); the helper scripts
`sc`, `kasrun`, `mcp.py`, `compare.py`, `lintsum.py`, `hookrun.py`, `sanitise.py`,
`copy-samples.sh` and the `calls-*.json` inputs. `{{LIVE}}/runs/verify/` holds the raw
outputs. In the kas build, the fetches left `build/downloads/` (curl's tarball and the
uninative shim) and `build/tmp/work/.../curl/` and `.../broken-fetch/`. `build/conf/` is as
kas writes it (the scratch layer was removed again by the next plain `kas shell`).

Since y1-build: the kas build directory holds the build of curl (14 GB at the end: `tmp/` with
`work/`, `pkgdata/`, `deploy/ipk/`, `deploy/cve/curl_cve.json`, `log/cve/` (the VEX
experiment's index file, left by the class), `stamps/`, `sysroots-components/`;
`sstate-cache/`; `downloads/` with every source of the 78 recipes). curl itself was rebuilt
at the end without the scratch bbappend (`Attempted 919 tasks of which 908 didn't need to
be rerun`), and `conf/bblayers.conf` and `conf/local.conf` are as kas writes them for
yocto-kiss's files (identical to the copies taken before, `runs/y1-build/conf-before/` and
`conf-after/`). `scratch/meta-scratch/recipes-test/` holds only `broken-fetch` again. The
raw outputs, the sous-chef configuration, cache and helper scripts are in
`{{LIVE}}/runs/y1-build/`.

Since y3-leftovers: `{{LIVE}}/runs/y3-leftovers/` holds the sous-chef configurations
(`sous-chef.toml`, `sous-chef-kas.toml`, `sous-chef-hooktest.toml`), the cache, the helper
scripts (`tools/`), `graph/` (`bitbake -g curl`), `hooktest/` (paths only, for the hook
timing), `patchwork/` and the raw outputs; its `tmp/` is empty again. In the kas build:
curl was rebuilt at the end without the scratch bbappend (from sstate, `918 didn't need to
be rerun`); its `T` keeps the failed `log.do_compile.1958029` and the interrupted
`log.do_configure.1910658`; `conf/` is identical to the copies taken before
(`runs/y3-leftovers/conf-before/`, `conf-after/`); `scratch/meta-scratch/recipes-test/`
holds only `broken-fetch`. BitBake itself rewrote `bitbake/lib/bb/pysh/pyshtables.py`
(its parser table) during these runs, and each plain `kas shell` rewrote the `.git`
metadata of the three clones (`repos_checkout`; the same pinned commits, checked in
`refs/heads/`); nothing else in `yocto-kiss/` outside `build/` changed
(`runs/y3-leftovers/tools/changed.py`).

To remove: `rm -r {{LIVE}}/scratch {{LIVE}}/runs/verify`, and if wanted
`{{LIVE}}/yocto-kiss/build/downloads` and `{{LIVE}}/yocto-kiss/build/tmp/work` (BitBake
recreates both). Outside the environment: one index directory written before the cache was
redirected, `~/.cache/sous-chef/e8c193705c567077/` (12 MB), which T9.2 could not delete
(the removal was refused by the permission system); delete it by hand.

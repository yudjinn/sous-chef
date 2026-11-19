# Differential test against BitBake (T8.1)

How often the static answer (`eval::evaluate`: own parser, index, replay in BitBake's order,
override finalisation) equals BitBake's real answer (`bitbake -e`), on a real workspace, and
what every difference is.

Measured on 2026-11-17 on the live environment ([live-environment.md](live-environment.md)):
yocto-kiss at `cc621c5d`, BitBake 2.18.0, OE-core / meta-arm / meta-yocto at the pins listed
there, MACHINE `dogbonedark`, DISTRO `kiss`, in the **bitbake-setup build**
(`bitbake-builds/kiss-wrynose-dogbonedark/build`; 7 layers: core, yoctobsp, yocto,
arm-toolchain, meta-arm, meta-kiss, meta-kiss-ti). sous-chef at `67909a3` (the first
measurement; the re-run after the fixes is under "After the w5 fixes").

## How to run it

```sh
export SOUS_CHEF_LIVE=~/Development/sous-chef-live
scripts/differential.sh                       # BitBake side: parse-only, ~5 s per recipe
cargo test --release --test differential -- --ignored --nocapture
```

- `scripts/differential.sh` sources the build's `init-build-env` and runs
  `bitbake-layers show-layers`, `bitbake-layers show-appends` and `bitbake -e <target>` for
  every target in its list, writing the raw output to `$SOUS_CHEF_LIVE/runs/differential/`
  (`env/<target>.env`, about 2 MB each) and one line per command to `commands.log`. A dump
  that exists is kept (`FORCE=1` reruns). Arguments name targets to run instead of the list.
  It runs nothing but parse-only commands. Use the bitbake-setup build, not the kas build:
  BitBake allows one client per build directory.
- `tests/differential.rs` (ignored unless run with `--ignored`; it returns at once when
  `SOUS_CHEF_LIVE` is unset) runs the script for any dump that is missing, discovers the
  workspace from the same build directory, refreshes an index kept in
  `$SOUS_CHEF_LIVE/runs/differential/cache` (never the user's cache), evaluates each target,
  compares, and writes the full report to `$SOUS_CHEF_LIVE/runs/differential/report.txt`.
  The comparison itself takes about 6 s.
- Recorded results, with the live prefix replaced by `{{LIVE}}`, are in
  `tests/fixtures/outputs/live/differential/`: `summary.txt` (per-target table, totals,
  variables by number of differences), `classes.tsv` (every compared value with its class and
  cause), `workspace.txt` (history files, layers, bbappend order, recipe files, configuration
  without a machine).
- The dumps are read with `src/bitbake/envdump.rs` (`parse_str`, T5.5) and each value goes
  through its `compare` first; the test's classes refine the `Agreement` (an `Equal` with no
  value on either side is `unset`; `NotPrinted`, a variable BitBake unexported or failed to
  expand, is `not-printed` and not counted; whitespace is checked before the flags). The
  minimal reader the first measurement used is gone (w5 fix, item 7); on the same dumps the
  switch changed no class (see "After the w5 fixes").
- `SOUS_CHEF_DIFF_NO_BITBAKE=1` makes a missing dump fail the test instead of running the
  script; `SOUS_CHEF_DIFF_DIR=<dir>` puts the index cache and `report.txt` there instead of
  `$SOUS_CHEF_LIVE/runs/differential` (the dumps are still read from there), so a run with
  another schema version does not rebuild a cache someone else uses.

## After the w5 fixes (branch `w5-fix-eval`)

Re-run on 2026-11-17 against the **same dumps** (no BitBake run: `SOUS_CHEF_DIFF_NO_BITBAKE=1`),
after the fixes of bugs 1 to 4, observation 6 and the reader switch:

| | Compared | Equal | Flagged (of which literal differs) | Known gap | Bug |
|---|---|---|---|---|---|
| Sample, explicit machine and distro: before | 1019 | 656 | 336 (22) | 27 | 0 |
| Sample, explicit machine and distro: after | 1019 | 656 | 336 (20) | 27 | 0 |
| `curl@default`: before / after | 38 / 38 | 22 / 24 | 13 / 13 | 1 / 1 | 2 / **0** |
| `sl@default`: before / after | 30 / 30 | 17 / 19 | 10 / 10 | 1 / 1 | 2 / **0** |
| `virtual/kernel@default`: before / after | 32 / 27 | 8 / 13 | 14 / 12 | 2 / 2 | 8 / **0** |

- **The `@default` probe has no bug left**: with no machine or distro given, the evaluation
  reads `conf/toolcfg.conf` and applies `OE_FRAGMENTS` (bug 1), so `MACHINE = dogbonedark`,
  `DISTRO = kiss`, `virtual/kernel` resolves to `linux-kiss` (`PreferredProvider`) and every
  `@default` row is the same as its explicit row. Two causes were behind bug 1, not one:
  the replay ignored `addfragments`, and the build directory was not on `BBPATH` (a
  bitbake-setup `bblayers.conf` sets no `BBPATH`; BitBake gets the build directory from the
  environment, `BBPATH=$BUILDDIR`), so neither `toolcfg.conf` nor `local.conf` was read.
- **Flagged literal differs 22 -> 20**: the two `DEPENDS` values of bug 3 (`linux-kiss`,
  `virtual/kernel`) now have the virtual providers rewritten (`binutils-cross-${@...}`,
  `gcc-cross-${@...}`) and match BitBake's text outside the python parts; they stay flagged
  because `DEPENDS` holds python from `base.bbclass`.
- **Everything else is unchanged**, class by class (`classes.tsv`), which also shows the
  envdump reader classifies these dumps as the old reader did. The 27 known gaps are the
  `LAYERDIR` symlink spelling (`FILE`, `FILESEXTRAPATHS`); the 20 flagged-literal-differs
  are the python cases listed under "Known gaps" below (15 named by BitBake's history, 5
  built from python-set variables or classes the replay could not inherit).
- **Files in the history**: same set for 828 of 832 names (was 784). `MACHINE` / `DISTRO`
  now name `toolcfg.conf` on both sides. Left: `kiss-image` `DEPENDS` and `B` (classes
  behind `${IMGCLASSES}` / `${SDK_CLASSES}`, unchanged), and `DEPENDS` of `linux-kiss` and
  `virtual/kernel`, where the static history ends with the rewrite, located at the
  `PREFERRED_PROVIDER_virtual/cross-*` assignment in `tcmode-default.inc`, while BitBake
  records it at `ast.py` (`[handleVirtRecipeProviders]`, a python operation the comparison
  leaves out).
- **Workspace level**: bbappend order agrees for all 26 recipes (`optee-os` fixed, bug 2);
  `evaluate("example")` is `NotFound`, as `bitbake -e example` is (bug 4); configuration
  without a machine: `MACHINE = dogbonedark`, `DISTRO = kiss`.

## After acc-overflag

Re-run on 2026-11-19 against the same dumps (`SOUS_CHEF_DIFF_NO_BITBAKE=1`,
`SOUS_CHEF_DIFF_DIR` under `runs/acc-overflag/`, release build) after anonymous-python
writes whose condition is false on static values stopped flagging (bitbake-semantics.md,
"When a value is flagged", "Anonymous python writes that cannot happen"):

| | compared | equal | of which flagged | flagged | flagged, literal differs | known gap | bug |
|---|---|---|---|---|---|---|---|
| before (f060e9e) | 1090 | 950 | 148 | 74 | 35 | 31 | 0 |
| after | 1090 | 950 | **88** | 74 | 35 | 31 | 0 |

Every class of `classes.tsv` is unchanged; only flags on values already equal moved. For an
equal value that still carries a flag, the cause column of `classes.tsv` now reads
`flagged: <causes>` (`DataStore::flag_causes`), and `report.txt` lists them under
"Equal, but flagged".

**The 148, by cause** (every one was a by-name write of anonymous python, or an override
decision on unknown `OVERRIDES` entries; none came from python in a replaced assignment, a
`?=` / `??=` that did not apply, an inactive override, a flag, or the recipe's anonymous
python as a whole, which the evaluator already did not flag):

| Cause | Values | Now |
|---|---|---|
| `base.bbclass` multimachine `d.setVar('PACKAGE_ARCH', ...)` (`S`, `B`, `WORKDIR`, `CFLAGS`, ... through `MULTIMACH_TARGET_SYS`) | 104 | 48 unflagged: `PACKAGE_ARCH` already `MACHINE_ARCH` (base-files, kiss-image, linux-kiss, packagegroup-core-boot, u-boot-kiss and the two `virtual/`: 34), no `file://` in `SRC_URI` (opencsd, sl, systemd-boot: 14). 56 stay: `FILESPATH` is `base_set_filespath(...)`, not evaluated, so the machine-directory test cannot be made (in the dumps no `FILESPATH` entry named `dogbonedark` exists for those 12 targets, so they should drop once `FILESPATH` is evaluated) |
| `multilib_script.bbclass` `d.appendVar("FILES:" + pkg, ...)` | 8 | unflagged: `if not d.getVar("MULTILIB_VARIANTS"): return` is true |
| `base.bbclass` `appendVar('PACKAGECONFIG_CONFARGS', extraconf)` (`EXTRA_OECMAKE` of expat, `EXTRA_OECONF` of ncurses, `EXTRA_OEMESON` of systemd-boot) | 3 (+1) | unflagged: no `PACKAGECONFIG` flag (`doc`, `vardepvalue` included) gives the list an item. quilt-native `EXTRA_OECONF` also waits on `OVERRIDES` and stays |
| `base.bbclass` `d.appendVar("EXTRANATIVEPATH", ...)` for `hg://` (python3) | 1 | unflagged: no `SRC_URI` entry has the hg scheme |
| Override decisions on unknown `OVERRIDES` entries (gmp-native, quilt-native, nativesdk-zlib: `BUILD_ARCH` is `os.uname()`) | 14 | stay: an unknown entry could be any override name |
| python3's manifest function (`d.appendVar('FILES:' + pypackage, ...)` from a JSON file) | 11 | stay: names and values come from a file the function reads |
| nativesdk `ClassExtender.rename_package_variables(PACKAGEVARS)` (nativesdk-zlib `LICENSE`, `SUMMARY`, ...) | 5 | stay (and `OVERRIDES` is unknown there too) |
| `kernel.bbclass` `d.setVar('FILES:' + kname + '-image-' + typelower, ...)` (`FILES:linux-kiss`, twice) | 2 | stay: the computed name is only known by its prefix `FILES:` |

## After x1-python

Re-run on 2026-11-18 against the same dumps (`SOUS_CHEF_DIFF_NO_BITBAKE=1`,
`SOUS_CHEF_DIFF_DIR` under `runs/x1-python/`, release build) after inline python of the
pure helpers is evaluated statically (bitbake-semantics.md, "Inline python evaluated
statically"):

| | compared | equal | of which flagged | flagged | flagged, literal differs | known gap | bug |
|---|---|---|---|---|---|---|---|
| before (w12-final) | 1090 | 702 | 147 | 332 | 25 | 31 | 0 |
| after | 1090 | 950 | 148 | 74 | 35 | 31 | 0 |
| x7-cleanup (same dumps, `runs/x7-cleanup/diff/`) | 1090 | 950 | 148 | 74 | 35 | 31 | 0 |

248 values moved from flagged to equal; **no value is unflagged and different** (0 bug,
and no new known gap). What made it: `MACHINEOVERRIDES` (`soc-family.inc`'s list
subscript and the `arch-*.inc` `bb.utils.contains('TUNE_FEATURES', ...)`),
`TRANSLATED_TARGET_ARCH`, `DISTROOVERRIDES` and `LIBCEXTENSION` are evaluated, so
`OVERRIDES` is known for every target recipe (13 of 13 entries for curl, 6 before, all
equal to BitBake's), and every value whose override decisions waited on it is certain
(`SRC_URI`, `FILES:*`, `PACKAGES`, `PACKAGECONFIG`, `RDEPENDS`); the tune's
`TUNE_PKGARCH` python (with `%`) and `MACHINE_ARCH` (with `bool()`) give `PACKAGE_ARCH` and so `WORKDIR`, `S`, `B`, `T` and
`CFLAGS`; `DISTRO_FEATURES` / `MACHINE_FEATURES` (`oe.utils.filter_default_features`).

Two changes were needed to keep 0 bugs. First, BitBake's second `need_overrides` pass is
only a check (fatal when it differs); when the first pass has unknown entries, the second
may know fewer, and that was taken for instability (`unresolved` `OVERRIDES` on every
override-dependent value) until the first pass is used in that case (bitbake-semantics.md,
"Overrides", "The two passes"). Second, three values became unflagged and short of what
BitBake has (`EXTRA_OECONF` of dropbear and python3, `EXTRA_OEMESON` of glib-2.0): they
append `${PACKAGECONFIG_CONFARGS}`, which `base.bbclass`'s anonymous python fills through
its local `appendVar(...)` wrapper; they had been flagged only for unrelated python. Writes
through such a wrapper now count as made by anonymous python, and the three are flagged
again.

What stays flagged (differences by variable): `DEPENDS`, `FILESPATH` (25 each: python
outside the subset, `get_base_dep(d)`, `base_set_filespath(...)`), `FILE` (25, the
path-spelling gap), `B`, `S`, `WORKDIR`, `CFLAGS` (3 each: the native and nativesdk
targets, where `BUILD_ARCH` is `os.uname()`), `EXTRA_OECONF` 9, `FILES` 8,
`PACKAGES` 7, `RDEPENDS` 5, `OVERRIDES` 3 (the native and nativesdk targets:
`TRANSLATED_TARGET_ARCH` from the host), `FILESEXTRAPATHS` 4. 148 equal values still carry
a flag (over-caution, not an error).

## After w12-final

Re-run on 2026-11-18 against the same dumps (`SOUS_CHEF_DIFF_NO_BITBAKE=1`,
`SOUS_CHEF_DIFF_DIR` under `runs/w12-final/`, release build) after the `nativesdk` and
multilib `PROVIDES` filters (bitbake-semantics.md, "Providers and versions"): 1090 values
compared, 702 equal (+0 after whitespace), 357 flagged (25 literal differs), 31 known gap,
0 bug. One row moved: `nativesdk-zlib` `PROVIDES`, equal after normalising whitespace
(and flagged) before, is now equal and not flagged: `nativesdk.bbclass`'s `suffix_filter`
is computed on the static value and joins the words with one space, as BitBake's does. Of
the equal values 147 carry a flag (148 before). Every other row of `classes.tsv` is
unchanged. (The workspace has no multilib configuration, so the multilib filter is not
exercised there.)

## After w11-final

Re-run on 2026-11-18 against the same dumps (`SOUS_CHEF_DIFF_NO_BITBAKE=1`,
`SOUS_CHEF_DIFF_DIR` under `runs/w11-final/`, release build) after the variant `PROVIDES`
filter, the variant class order and the new skip notes: 1090 values compared, 701 equal
(+1 after whitespace), 357 flagged (25 literal differs), 31 known gap, 0 bug. One row
moved: `gmp-native` `PROVIDES`, equal after normalising whitespace before, is now equal:
`native_virtclass_handler`'s `native_filter` is computed on the static value and joins the
words with one space, as BitBake's does (bitbake-semantics.md, "Providers and versions").
Of the equal values 148 carry a flag (149 before: `PROVIDES` of the native variant is no
longer flagged). Every other row of `classes.tsv` is unchanged.

## After w10-eval

Re-run on 2026-11-18 against the same dumps (`SOUS_CHEF_DIFF_NO_BITBAKE=1`,
`SOUS_CHEF_DIFF_DIR` under `runs/w10-eval/`, release build) after class-extended variants
became evaluable (bitbake-semantics.md, "Class-extended variants"):

| | Compared | Equal (+ after whitespace) | Flagged (of which literal differs) | Known gap | Bug |
|---|---|---|---|---|---|
| Sample, explicit machine and distro: before | 1019 | 656 | 336 (20) | 27 | 0 |
| Sample, explicit machine and distro: after | 1090 | 700 (+2) | 357 (25) | 31 | 0 |
| `gmp-native` (new) | 36 | 22 (+1) | 11 (3) | 2 | 0 |
| `nativesdk-zlib` (new) | 35 | 22 (+1) | 10 (2) | 2 | 0 |

- **The two targets that were `NotFound` are evaluated and compared**, with no unflagged
  difference. Equal: `PN` (`gmp-native`, `nativesdk-zlib`: the `extendfunc` and
  `nativesdk_virtclass_handler` renames), `PV`, `PR`, `LICENSE`, `INHERIT`, `TMPDIR`,
  `BBCLASSEXTEND`, `MACHINEOVERRIDES` (`""` from the class), `SRC_URI`, the recipes' own
  variables (`nativesdk-zlib` `EXTRA_OECONF` too); `PROVIDES` equal after normalising
  whitespace. Flagged: `DEPENDS`, `PACKAGES`, `FILES:<pn>` (the handlers' filters and
  `nativesdk.bbclass`'s `ClassExtender`, plus python already in the base), `OVERRIDES`,
  `DISTROOVERRIDES`, `S`, `B`, `WORKDIR`, `CFLAGS`, `FILESPATH` (as for every target), and
  `gmp-native` `EXTRA_OECONF`. Of the 700 equal values 149 carry a flag (135 before). Literal
  differs: `gmp-native` `PACKAGES` (BitBake's `native_filter` renames), `OVERRIDES`,
  `EXTRA_OECONF`; `nativesdk-zlib` `DEPENDS` (the `virtual/nativesdk-*` and
  `gcc-crosssdk` names the class filters and virtual providers give), `OVERRIDES`.
  `OVERRIDES` carries `pn-<variant>`, `class-native` / `class-nativesdk` and
  `toolchain-gcc` on both sides.
- **Known gaps** per variant: `FILE` (the `LAYERDIR` spelling, as for every target) and the
  new `variant-layername`: `bitbake -e <variant>` parses the virtual file name, which no
  `BBFILE_PATTERN` matches, so BitBake's `FILE_LAYERNAME` is `None` there (and its
  `OVERRIDES` keeps a literal `layer-${FILE_LAYERNAME}`), while a build, and sous-chef,
  give the layer's collection (`core`).
- **No other class moved**: the 23 earlier targets and the `@default` probe have the same
  rows in `classes.tsv`; only the 76 rows of the two variants were added. Files in the
  history: same set for 896 of 898 names (the 66 new names all agree).

## After w9-final

Re-run on 2026-11-18 against the same dumps (`SOUS_CHEF_DIFF_NO_BITBAKE=1`,
`SOUS_CHEF_DIFF_DIR` under `runs/w9-final/`, release build) at the end of the code part of
branch `w9-final` (the `.inc` inherit contexts in the index, one statement-row conversion,
stale job outputs at start), which also covers w7 (`addpylib`, inherit contexts in
`classes/`, `BBFILES_DYNAMIC` through includes) and the end of w8:

| | Compared | Equal | Flagged (of which literal differs) | Known gap | Bug |
|---|---|---|---|---|---|
| Sample, explicit machine and distro | 1019 | 656 | 336 (20) | 27 | 0 |

- **No class moved**; `tests/fixtures/outputs/live/differential/summary.txt` and
  `classes.tsv`, which the test rewrites, are unchanged. Files in the history: same set
  for 830 of 832 names, as after w6-fix.

## After w8-eval

Re-run on 2026-11-18 against the same dumps (`SOUS_CHEF_DIFF_NO_BITBAKE=1`,
`SOUS_CHEF_DIFF_DIR` under `runs/w8-eval/`), after the virtual-provider change (task
`depends` flags, `DEPENDS` always stored with single spaces, unset `DEPENDS` = `""`) and
again at the end of branch `w8-eval`:

| | Compared | Equal | Flagged (of which literal differs) | Known gap | Bug |
|---|---|---|---|---|---|
| Sample, explicit machine and distro: before | 1019 | 656 | 336 (20) | 27 | 0 |
| Sample, explicit machine and distro: after | 1019 | 656 | 336 (20) | 27 | 0 |

- **No class moved.** On yocto-kiss every sampled `DEPENDS` holds python, so the
  whitespace normalisation (done only for a certain value) does not apply, and the
  comparison normalises whitespace anyway; task flags are not compared.

## After w6-fix

Re-run on 2026-11-17 against the same dumps (`SOUS_CHEF_DIFF_NO_BITBAKE=1`,
`SOUS_CHEF_DIFF_DIR` under `runs/w6-fix/`), at the end of branch `w6-fix`:

| | Compared | Equal | Flagged (of which literal differs) | Known gap | Bug |
|---|---|---|---|---|---|
| Sample, explicit machine and distro: before | 1019 | 656 | 336 (20) | 27 | 0 |
| Sample, explicit machine and distro: after | 1019 | 656 | 336 (20) | 27 | 0 |

- **No class moved** (`classes.tsv` and `summary.txt` unchanged).
- **Files in the history: same set for 830 of 832 names (was 828).** The rewrite of virtual
  providers in `DEPENDS` is recorded as its own kind of operation
  (`OpKind::VirtualProviderRewrite`) and, like BitBake's `[handleVirtRecipeProviders]`
  python operation, left out of the comparison, so `tcmode-default.inc` no longer appears
  only in the static history of `linux-kiss` and `virtual/kernel` `DEPENDS`. Left:
  `kiss-image` `DEPENDS` and `B` (classes behind `${IMGCLASSES}` / `${SDK_CLASSES}`).
- `gmp-native` and `nativesdk-zlib` are still not evaluated (the evaluator is called
  directly); asked for through a tool they are now an error naming the base recipe
  (bitbake-semantics.md, "What the static evaluator cannot do").

## After w5-fix-eval2

Re-run on 2026-11-17 against the same dumps (`SOUS_CHEF_DIFF_NO_BITBAKE=1`,
`SOUS_CHEF_DIFF_DIR` under `runs/fix-eval2/`), before and after the branch:

| | Compared | Equal | Flagged (of which literal differs) | Known gap | Bug |
|---|---|---|---|---|---|
| Sample, explicit machine and distro: before | 1019 | 656 | 336 (20) | 27 | 0 |
| Sample, explicit machine and distro: after | 1019 | 656 | 336 (20) | 27 | 0 |

- **`inherit_defer toolchain/${TOOLCHAIN}` is followed** (live verification B5;
  bitbake-semantics.md, "When a deferred inherit's argument is expanded"): `TOOLCHAIN`
  expands to `gcc`, its uncertainty came only from inactive `class-*` variants of
  `PREFERRED_TOOLCHAIN` while `OVERRIDES` holds python, so `classes/toolchain/gcc.bbclass` is
  read and what it sets is flagged instead of missing. Flagged values that list `TCOVERRIDE`
  in `unresolved`: **156 -> 0**. Unfollowed references: one fewer in each of the 19 targets
  that inherit through `base.bbclass:36` (curl 3 -> 2, ...; linux-kiss, u-boot-kiss and their
  `virtual/` aliases set `TOOLCHAIN = "gcc"` themselves and already had it).
- **No class moved** (`classes.tsv` unchanged): every value that listed `TCOVERRIDE` is also
  flagged for python (`OVERRIDES` still holds python from `TARGET_OS`,
  `TRANSLATED_TARGET_ARCH`, the tune's `MACHINEOVERRIDES` and `DISTROOVERRIDES`), and the
  variables `gcc.bbclass` sets (`CC`, `CXX`, `LD`, `OBJDUMP`, ... and `TCOVERRIDE`) are not
  in the compared set; `OVERRIDES`, which uses `TCOVERRIDE`, stays flagged for python. The
  change is in what the flags say, not in how many values carry one.
- **`OVERRIDES` of curl**: BitBake has 13 entries
  (`linux-gnueabi:arm:pn-curl:layer-core:armv7a:ti-soc:ti33x:dogbonedark:kiss:class-target:toolchain-gcc:libc-glibc:forcevariable`).
  Known cleanly now: **6** (`pn-curl`, `layer-core`, `class-target`, `toolchain-gcc`,
  `libc-glibc`, `forcevariable`; was 5, without `toolchain-gcc`). The other 7 are inside 4
  unevaluated entries, each kept whole and never taken as an override name (B4,
  bitbake-semantics.md "Splitting list values"): `linux${@...}eabi` (`TARGET_OS`) for
  `linux-gnueabi`, `${@d.getVar('TARGET_ARCH')...}` (`TRANSLATED_TARGET_ARCH`) for `arm`, the
  tune and SoC python of `MACHINEOVERRIDES` followed by `dogbonedark` for `armv7a`,
  `ti-soc`, `ti33x`, `dogbonedark`, and `${@d.getVar('DISTRO') or ''}` (`DISTROOVERRIDES`)
  for `kiss`. What stops them is python only; `ti33x`, which the naive split used to find by
  accident, is no longer claimed.

## Status after the fix rounds (2026-11-17)

All four bugs of the first measurement and the `is_function` observation are fixed, and the
`@default` probe has no bug left (12 -> 0): see the two sections above. On the sample with
the build's machine and distro, static evaluation equalled BitBake for **656 of 1019 values
(64%)**; the other 363 differed and said so (336 carried a python or unresolved flag) or
were the documented `LAYERDIR` spelling gap (27); **no value differed without a flag or a
documented cause**. Since w5-fix-eval2 no flagged value lists `TCOVERRIDE` (156 -> 0) and
20 flagged values (was 22) differ even outside their unevaluated parts.

Open, as documented gaps rather than bugs: `BBCLASSEXTEND` variants (`gmp-native`,
`nativesdk-zlib` were refused, not evaluated; evaluated since w10, see "After w10-eval");
`LAYERDIR` is the canonical path, not the
`BBLAYERS` text; anonymous python and python in `OVERRIDES` (behind most flags). (The task
`depends` flags and the whitespace of an unchanged `DEPENDS` in the virtual-provider
rewrite, open here at first, were done in w8: "After w8-eval".) Not measured: other
machines, distros and releases; functions and flags.

**The test fails only on what the milestone names:** a value classified **bug** on a tracked
variable (`SRC_URI`, `DEPENDS`, `RDEPENDS:*`, `FILES:*`, `PACKAGECONFIG`, `EXTRA_OECONF`,
`EXTRA_OECMAKE`, `S`, `B`, `PV`, `LICENSE`, `OVERRIDES`, `FILESPATH`, `PREFERRED_PROVIDER_*`)
in the sample evaluated with the build's machine and distro. It passes today. The expected
classes never fail it, and the `@default` probe (below) is reported, not asserted.

## What is compared

**Targets (25).** Ordinary target recipes `curl` (autotools, `PACKAGECONFIG`), `zlib`,
`openssl`, `ncurses`, `bash`, `expat` (cmake), `glib-2.0` and `dbus` (meson), `util-linux`,
`python3`, `busybox` (bbappend from meta-poky), `dropbear`, `base-files` (bbappend from
meta-poky); `systemd-boot` (bbappend from meta-arm); `opencsd` (meta-arm); the
workspace's own recipes `sl`, `linux-kiss` (kernel, bbappend from meta-kiss-ti),
`u-boot-kiss` (bootloader) and `kiss-image` (image; `bitbake -e` of an image only parses); a
packagegroup `packagegroup-core-boot`; `quilt-native` (a `-native` recipe file);
`gmp-native` and `nativesdk-zlib` (`BBCLASSEXTEND` variants); and the providers
`virtual/kernel` and `virtual/bootloader` (BitBake's `FILE` against sous-chef's provider
selection). `inherit_defer` is exercised by every target (base, kernel, image,
packagegroup, useradd classes use it). meta-kiss and meta-kiss-ti are the workspace's own
layers in this build (meta-kiss-amd, -nxp and -st are not in `BBLAYERS` for this machine).

**Variables.** For every target: `PN`, `PV`, `PR`, `FILE`, `FILE_LAYERNAME`, `SRC_URI`,
`DEPENDS`, `RDEPENDS:${PN}`, `PACKAGECONFIG`, `PACKAGES`, `FILES:${PN}`, `OVERRIDES`,
`MACHINEOVERRIDES`, `DISTROOVERRIDES`, `S`, `B`, `WORKDIR`, `LICENSE`, `INHERIT`,
`PROVIDES`, `EXTRA_OECONF`, `EXTRA_OECMAKE`, `CFLAGS`, `FILESPATH`, `MACHINE`, `DISTRO`,
`TMPDIR`; plus every variable the recipe's own files (the `.bb`, the `.inc` files it
includes, its bbappends; not classes, not configuration) assign: the base name, or
`VAR:${PN}...` for a package override. Functions are not compared (the static side stores
them as pointers). `${PN}` in a name is expanded with each side's `PN`. A name unset on both
sides is listed as `unset` and not counted.

**Machine and distro.** In this build MACHINE and DISTRO are not in `local.conf`; they come
from `OE_FRAGMENTS += "distro/kiss machine/dogbonedark"` in `conf/toolcfg.conf` (see bug 1).
The sample is evaluated with `Target { machine: "dogbonedark", distro: "kiss" }`, as BitBake
ran. Three targets are also evaluated with `machine` and `distro` left `None`, as a tool call
without those arguments does (`curl@default`, `sl@default`, `virtual/kernel@default`).

## Classes

Each compared value is in exactly one class, decided in this order:

| Class | Meaning |
|---|---|
| **equal** | the same text |
| **equal-ws** | the same after collapsing whitespace (an unset value equals `""`) |
| **flagged** | differs, and the static value carries `unevaluated_python` or a non-empty `unresolved`: sous-chef said it may be wrong. Sub-class **flagged-literal-differs**: even the text outside the unevaluated parts disagrees with BitBake (each `${@...}` / `${VAR}` left in the static text is allowed to stand for any text) |
| **known gap** | differs, not flagged, and the cause is a documented limitation, named |
| **bug** | differs, not flagged, no documented cause: a wrong answer presented as a fact |

Causes are found by rule, not by hand: a difference that disappears when the
`layers/<x>` symlinks of the bitbake-setup build are mapped to their canonical targets is
`layerdir-canonical`; otherwise the variables referenced by BitBake's pre-expansion value
and the static raw value are classified first and the first that differs gives its class
(`via ${X}`); otherwise a BitBake history operation made by python at parse time (`[func]`
in the history, other than BitBake's `expandKeys`) makes it `python-at-parse`; a `DEPENDS`
that still holds a word of `BB_RECIPE_VIRTUAL_PROVIDERS` is bug 3; anything else is a bug
and the report gives both sides' last operation.

## Results

### Per target (explicit machine and distro)

| Target | Compared | Equal | Flagged (of which literal differs) | Known gap | Bug |
|---|---|---|---|---|---|
| curl | 38 | 24 | 13 (1) | 1 | 0 |
| sl | 30 | 19 | 10 | 1 | 0 |
| linux-kiss | 27 | 13 | 12 (2) | 2 | 0 |
| u-boot-kiss | 46 | 29 | 16 (1) | 1 | 0 |
| kiss-image | 27 | 16 | 10 (1) | 1 | 0 |
| packagegroup-core-boot | 31 | 15 | 15 (1) | 1 | 0 |
| zlib | 35 | 22 | 12 | 1 | 0 |
| openssl | 48 | 34 | 13 (1) | 1 | 0 |
| busybox | 64 | 41 | 22 (1) | 1 | 0 |
| dropbear | 46 | 31 | 14 (1) | 1 | 0 |
| ncurses | 54 | 40 | 13 (1) | 1 | 0 |
| dbus | 45 | 28 | 16 (1) | 1 | 0 |
| systemd-boot | 42 | 26 | 14 | 2 | 0 |
| util-linux | 98 | 76 | 20 | 2 | 0 |
| quilt-native | 38 | 25 | 12 | 1 | 0 |
| expat | 36 | 22 | 13 | 1 | 0 |
| glib-2.0 | 53 | 36 | 16 | 1 | 0 |
| base-files | 46 | 30 | 15 | 1 | 0 |
| python3 | 69 | 41 | 27 (8) | 1 | 0 |
| bash | 42 | 27 | 14 | 1 | 0 |
| opencsd | 31 | 19 | 11 | 1 | 0 |
| virtual/kernel (linux-kiss) | 27 | 13 | 12 (2) | 2 | 0 |
| virtual/bootloader (u-boot-kiss) | 46 | 29 | 16 (1) | 1 | 0 |
| gmp-native | 0 | | | not evaluable: `BBCLASSEXTEND` (evaluated since w10) | |
| nativesdk-zlib | 0 | | | not evaluable: `BBCLASSEXTEND` (evaluated since w10) | |
| **Total** | **1019** | **656** | **336 (22)** | **27** | **0** |

No value was equal only after whitespace normalisation. 58 names were unset on both sides
and are not counted. Of the 656 equal values, 135 carried a flag although they were right
(over-caution, not an error).

By variable, over the 23 evaluable targets: `PN`, `PV`, `PR`, `FILE_LAYERNAME`, `LICENSE`,
`INHERIT`, `PROVIDES`, `TMPDIR` were equal for all 23 (and `MACHINE`, `DISTRO`, which the
target sets); `SRC_URI` equal 16, flagged 7; `EXTRA_OECONF` equal 14, flagged 9 (3 literal
differs); `PACKAGES` equal 9, flagged 14 (4); `FILES:*` equal 37, flagged 27 (3);
`RDEPENDS:*` equal 26, flagged 10 (2); `PACKAGECONFIG` equal 2, flagged 7 (unset in 14);
`DEPENDS` flagged in all 23 (8 literal differs); `OVERRIDES`, `DISTROOVERRIDES`, `S`, `B`,
`WORKDIR`, `CFLAGS`, `FILESPATH` flagged in all 23; `MACHINEOVERRIDES` flagged 22, equal 1;
`FILE` a known gap in all 23. Variables the recipes' own files assign: 320 equal, 56 flagged
(2 literal differs), 4 known gap.

**Files in the history** (for the 832 compared names without an override): the set of files
in BitBake's history (statements only; python operations and flag operations left out)
equals the set of files in the static history for 784. The 48 others: `MACHINE` and
`DISTRO` in every target, where BitBake's history names `conf/toolcfg.conf` (the fragment)
and the static side has the explicit target instead (46); and two in `kiss-image`
(`DEPENDS` from `image_types_wic.bbclass`, `B` from `populate_sdk_ext.bbclass`), classes the
replay did not inherit because their `inherit_defer` argument (`${IMGCLASSES}`,
`${SDK_CLASSES}`) needs python; the replay lists both in its unfollowed references and the
two values are flagged.

### The `@default` probe (machine and distro not given)

| Target | Compared | Equal | Flagged | Known gap | Bug |
|---|---|---|---|---|---|
| curl@default | 38 | 22 | 13 | 1 | 2 (`MACHINE`, `DISTRO`) |
| sl@default | 30 | 17 | 10 | 1 | 2 (`MACHINE`, `DISTRO`) |
| virtual/kernel@default | 32 | 8 | 14 | 2 | 8 (`PN`, `PV`, `SRC_URI`, `PROVIDES`, `MACHINE`, `DISTRO`, `LIC_FILES_CHKSUM`, `PACKAGES_DYNAMIC`) |

All 12 come from bug 1.

## Bugs (unflagged wrong answers), most serious first

All four are **fixed on branch `w5-fix-eval`** (rules and sources in bitbake-semantics.md:
"Configuration fragments", "`BBPATH` from the environment", "The sort is a string sort",
"Recipe virtual providers", "A `.bb` or `.bbappend` that no `BBFILES` glob matches"); the
text below is the first measurement's description.

**1. `addfragments` (OE configuration fragments) is not replayed, so a bitbake-setup build
has the wrong MACHINE and DISTRO.** Wrynose's `bitbake-setup` writes MACHINE and DISTRO as
fragments: `conf/toolcfg.conf` has `OE_FRAGMENTS += "distro/kiss machine/dogbonedark"`, and
`meta/conf/bitbake.conf:822` has `addfragments ${OE_FRAGMENTS_PREFIX} OE_FRAGMENTS
OE_FRAGMENTS_METADATA_VARS OE_FRAGMENTS_BUILTIN` with `OE_FRAGMENTS_BUILTIN ?=
"machine:MACHINE distro:DISTRO"` (line 821), so BitBake sets `MACHINE = "dogbonedark"` and
`DISTRO = "kiss"` (its history: `set .../build/conf/toolcfg.conf:6 "kiss (OE_FRAGMENTS
contains "distro/kiss")"`). The parser recognises `addfragments` but the replay ignores it.
Reproduction: in this build, `eval::evaluate` with `Target { recipe: None, machine: None,
distro: None }` gives `MACHINE` = unset and `DISTRO` = `"nodistro"`, `unevaluated_python:
false`, `unresolved: []`; `bitbake -e` gives `dogbonedark` and `kiss`. Consequences in the
probe: `virtual/kernel` resolves to `linux-dummy` (`FirstOfSeveral`) instead of `linux-kiss`
(`PREFERRED_PROVIDER_virtual/kernel ?= "linux-kiss"` is in `dogbonedark.conf`, which is never
read), and `PN` = `linux-dummy`, `PV` = `1.0`, `SRC_URI` = `file://COPYING.GPL` are
presented as facts. Every tool call without `machine` / `distro` in such a build is
affected, and `deps`, which has no machine argument, always is; so is lint's recipe-scope
evaluation. Named fragments (`conf/fragments/<layer>/<name>.conf` files) are not read
either. The kas build is not affected (kas writes `MACHINE ??=` into `local.conf`). Not in
any "Open items" list or in bitbake-semantics.md.

**2. Files matched by one `BBFILES` glob are ordered by path components, not as strings,
so bbappends can apply in the wrong order.** BitBake's `collect_bbfiles` (`cooker.py`)
does `for g in sorted(globbed)`: a Python string sort, in which `-` (0x2D) sorts before `/`
(0x2F). `workspace::resolve::recipe_files` sorts the glob results as `Vec<PathBuf>`
(`m.sort()`, `src/workspace/resolve.rs:445`), and `Path` compares component by component,
so `optee` sorts before `optee-ftpm`. Reproduction from `bitbake-layers show-appends` in this
workspace: for `optee-os_4.9.0.bb` and `optee-os_git.bb` BitBake applies
`meta-arm/recipes-security/optee-ftpm/optee-os_%.bbappend` then
`meta-arm/recipes-security/optee/optee-os_%.bbappend`; `query::appends_for` returns them the
other way round. Minimal case: one layer with `recipes-x/foo/a_%.bbappend` and
`recipes-x/foo-bar/a_%.bbappend` that both assign `X`; BitBake's final `X` is the one from
`foo/` (applied last), sous-chef's the one from `foo-bar/`, unflagged. No value differed in
this sample because both optee recipes are skipped on this machine. The other 24 recipes
with bbappends have the same order on both sides. bitbake-semantics.md describes the sort
correctly ("expanded with `glob.glob` ... and sorted"); the code does not follow it.

**3. BitBake 2.18 rewrites virtual providers in `DEPENDS`; the replay does not.**
`bb.parse.ast.handleVirtRecipeProviders` (run from `finalize`) replaces each `DEPENDS` word
listed in `BB_RECIPE_VIRTUAL_PROVIDERS` (here `virtual/cross-cc virtual/cross-c++
virtual/cross-binutils virtual/nativesdk-cross-cc virtual/nativesdk-cross-c++
virtual/nativesdk-cross-binutils`) with the value of `PREFERRED_PROVIDER_<word>`, and does
the same for the recipe names in task `depends` flags. Reproduction: `linux-kiss` `DEPENDS`
is `pkgconf-native binutils-cross-arm gcc-cross-arm kmod-native bc-native bison-native` in
BitBake, while the static value still has `virtual/cross-binutils virtual/cross-cc`. In
this sample the value was also flagged every time (`DEPENDS` always holds python from
`base.bbclass`), so it counts as flagged-literal-differs, not as a bug; a recipe whose
`DEPENDS` has no python would get a wrong unflagged answer. Neither the variable nor the
function is mentioned in docs/ or src/.

**4. A `.bb` that no `BBFILES` glob matches is indexed and evaluated as a recipe.** The
index has 1035 `.bb` files, `recipe_files` (the `BBFILES` globs) 1034, which is the number
BitBake parses. The extra one is OE-core's template `meta/lib/bblayers/templates/example.bb`:
`eval::evaluate` of recipe `example` succeeds and returns that file, while `bitbake -e
example` fails with `ERROR: Nothing PROVIDES 'example'`. Low impact (an unusual name), but
the `recipe`, `var` and lint tools would answer for a recipe BitBake does not have. The
index also holds `meta-arm/dynamic-layers/meta-tpm2/.../tpm2-tools_5.7.bbappend`, which is
under `BBFILES_DYNAMIC` for a layer that is absent; that one is a known gap
(`BBFILES_DYNAMIC` not reproduced).

Smaller observation, not counted (fixed on `w5-fix-eval`: `is_function` also knows names a
function statement wrote, BitBake's own `func` flag is lost by `renameVar`; the test no
longer recognises functions by shape): `DataStore::is_function` returns false for
`pkg_postinst:busybox`, `pkg_postrm:dropbear` and similar package-specific functions whose
static value is a pointer list; the test recognises functions by the value's shape instead.

## Known gaps that occurred

| Gap (where documented) | Occurrences |
|---|---|
| `LAYERDIR` is the canonical layer root, not the `BBLAYERS` text (wave 3 "Open items", Replay) | 27 unflagged values: `FILE` in all 23 targets, `FILESEXTRAPATHS` in 4 (linux-kiss, virtual/kernel, systemd-boot, util-linux). bitbake-setup's `layers/` entries are symlinks; BitBake keeps the symlinked path, sous-chef the target. The files are the same. Paths that BitBake prints through `layers/` (also in `FILESPATH`, `THISDIR`) will not match textually. |
| `BBCLASSEXTEND` variants not resolved (wave 3 Replay, wave 4 Provider resolution; closed in w10) | 2 targets: `gmp-native`, `nativesdk-zlib` gave `NotFound` ("recipe gmp-native; list recipes with the recipe tool"). A refusal, not a wrong answer. Since w10 both are evaluated ("After w10-eval"). |
| Python in `OVERRIDES` (wave 2) | Behind most flags: `OVERRIDES` itself is flagged in all 23 targets (`TARGET_OS`, `TRANSLATED_TARGET_ARCH`, the tune `MACHINEOVERRIDES` are python). (The rest of this cell is the first measurement; since w5-fix-eval2 the toolchain class is followed, see "After w5-fix-eval2".) The replay also reports `inherit_defer toolchain/${TOOLCHAIN}` (base.bbclass:36; `TOOLCHAIN ??= "${PREFERRED_TOOLCHAIN}"`, which has `class-*` override variants) as needing python in 19 of the 23 targets (not in linux-kiss, u-boot-kiss and their two `virtual/` aliases, where `kernel-arch.bbclass:84` sets `TOOLCHAIN = "gcc"`), so `toolchain/gcc.bbclass` is not replayed and `TCOVERRIDE` stays unresolved (which part of the chain is python was not traced further): 156 of the 336 flagged values list `TCOVERRIDE` in `unresolved`. |
| Anonymous python and `d.setVar` in classes not run (wave 3 Replay; bitbake-semantics.md "What the static evaluator cannot do") | Every target has 10 to 17 anonymous functions (7 to 12 opaque). Of the 22 flagged-literal-differs values, the BitBake history names the python that made the difference for 15: `kernel.bbclass` / `kernel-devicetree.bbclass` adding `kernel-image-zimage kernel-devicetree` to `PACKAGES`; `packagegroup.bbclass` adding `-dbg -dev`; `base.bbclass:508` (`appendVar`, `PACKAGECONFIG` dependencies) and `update-alternatives.bbclass:78` (`ua_extend_depends`) to `DEPENDS`; python3's manifest function to `PACKAGES`, `FILES:*`, `RDEPENDS:*`; busybox's anonymous function to `configmangle`. Of the other 7, 2 are bug 3 (`linux-kiss` and `virtual/kernel` `DEPENDS`) and 5 are values built from python-set variables or classes the replay could not inherit (`EXTRA_OECONF` of curl, dropbear and python3, `EXTRA_OEMESON` of dbus, `DEPENDS` of kiss-image). No unflagged value was caused by python. |
| `inherit` / `inherit_defer` whose argument needs python (bitbake-semantics.md) | Every target: `require ${@...multiconfig...}` and `include conf/machine-sdk/${SDKMACHINE}.conf` (bitbake.conf:824, 826), `inherit_defer toolchain/${TOOLCHAIN}`; `kiss-image` also `${IMGCLASSES}` and `${SDK_CLASSES}`. All reported in `ReplayNotes.references`. |
| `addtask` not replayed, `BB_DEFER_BBCLASSES` | Not observable in these variables. |

## Workspace-level comparisons

- **Layers and priorities** (`bitbake-layers show-layers`): agree. 7 layers, same order,
  same collection names and priorities (core 5, yoctobsp 5, yocto 5, arm-toolchain 5,
  meta-arm 5, meta-kiss 9, meta-kiss-ti 9); paths agree after mapping the `layers/` symlinks.
- **bbappend order** (`bitbake-layers show-appends`): 24 recipes agree, 2 differ
  (`optee-os_4.9.0.bb`, `optee-os_git.bb`: bug 2). `show-appends` lists `u-boot_2026.01.bb`
  and `linux-yocto_6.18.bb` twice with the same appends; the second listing is ignored.
- **Provider choices** (`FILE` in the dump): `virtual/kernel` -> `linux-kiss_7.0.bb` and
  `virtual/bootloader` -> `u-boot-kiss_git.bb`, the same files as BitBake (selected through
  `PREFERRED_PROVIDER_*` from `dogbonedark.conf`), for all 23 recipe targets the same file
  as BitBake. Without an explicit machine, `virtual/kernel` is wrong (bug 1).
- **Recipe files**: 1034 on both sides once the `BBFILES` globs are applied; the index
  holds one more (bug 4).
- **Skip checks** (w10, `tests/skip_live.rs`, ignored without `SOUS_CHEF_LIVE`; no BitBake
  run): the `COMPATIBLE_MACHINE` / `COMPATIBLE_HOST` / `SKIP_RECIPE` checks the skip notes
  make (`eval::compatible_machine`, `eval::compatible_host`) run on every collected recipe
  (evaluated statically for `dogbonedark` / `kiss`) with the `MACHINEOVERRIDES` and
  `HOST_SYS` BitBake computed, taken from the dumps as a check, not as an input: `curl`'s
  (`armv7a:ti-soc:ti33x:dogbonedark`, `arm-oe-linux-gnueabi`) for target recipes,
  `gmp-native`'s (`""`, `x86_64-linux`) for those that inherit `native`,
  `nativesdk-zlib`'s (`""`, `x86_64-oesdk-linux`) for `nativesdk`; the 20 cross recipes are
  left out (no dump has their values). Of 1014 recipes, **83 would be skipped** (65 by
  `COMPATIBLE_HOST`, 18 by `COMPATIBLE_MACHINE`, none by `SKIP_RECIPE`), 30 cannot be
  decided statically (`COMPATIBLE_MACHINE` / `COMPATIBLE_HOST` with python or override
  variants on python `OVERRIDES`; two lookahead expressions Rust's `regex` cannot compile,
  `gn`, `ghostscript`). Against BitBake: **no recipe judged skipped was parsed by BitBake**
  (the 20 recipe files with a target `bitbake -e` dump), and of the 24 recipe files
  `show-appends` lists, 16 agree (`trusted-firmware-a` x4, `libts`, `ts-sp-smm-gateway`,
  `linux-yocto-rt`, `linux-yocto-tiny` skipped on both sides; `linux-kiss`, `base-files`,
  `busybox`, `systemd-boot`, ... parsed on both sides). The other 8 BitBake skipped where
  these checks do not: 5 have a `COMPATIBLE_MACHINE` that is not static here
  (`linux-yocto`, `linux-yocto-dev`, `edk2-firmware`, `optee-os` x2), and 3 are skipped by
  checks the notes do not model: `u-boot_2026.01.bb` and `musl_git.bb` provide
  `virtual/bootloader` / `virtual/libc` whose `PREFERRED_PROVIDER` names another recipe
  (OE-core `base_eventhandler` on `RecipeParsed` skips such providers), `systemd_259.5.bb`
  has `REQUIRED_DISTRO_FEATURES` (`features_check.bbclass`); the source names these causes,
  they were not confirmed recipe by recipe. BitBake's total, 435 skipped, counts variants
  and every reason, so it is not comparable with the 83. The notes as they stand (static
  values only, `MACHINEOVERRIDES` holding python): "would skip" on no recipe, "may skip" on
  124, among them all 83; no recipe the real values skip lacks a note.
  **w11**: the two reasons are modelled (bitbake-semantics.md, "Skipped recipes") and the
  test runs them with the dumps' `DISTRO_FEATURES`, `MACHINE_FEATURES`,
  `COMBINED_FEATURES`, `IMAGE_FEATURES`, `TUNE_FEATURES`, `PREFERRED_PROVIDER_*` and
  `BB_MULTI_PROVIDER_ALLOWED`: 237 recipes judged skipped, still none parsed by BitBake;
  `show-appends` agreement 21 of 24 (u-boot, musl, systemd, linux-yocto-dev and
  edk2-firmware now agree); the 3 left (`linux-yocto_6.18.bb`, `optee-os` x2) have a
  `COMPATIBLE_MACHINE` that is not static. The static notes: "would skip" on 13 recipes
  (none parsed by BitBake; the test now fails if one is), "may skip" on 274.

## How far to trust static mode

On this workspace, with the machine and distro given. The counts in this section are the
23-target sample after the w5 fixes; the current totals, with the two `BBCLASSEXTEND`
variants and the later rounds ("After x1-python"), are 1090 values compared, 950 equal
(87%), 109 flagged, 31 known gap (29 path spellings, 2 `FILE_LAYERNAME` of a variant) and
0 bug; since acc-overflag 88 of the 950 equal values carry a flag (148 before).
(The README quotes the w12 numbers: 702 equal, 357 flagged.)

- **Nothing was wrong without a warning.** 1019 values compared, 0 bugs: every value that
  differed from BitBake either carried `unevaluated_python` / `unresolved` (336) or was a
  documented path-spelling difference (27). That is the property sous-chef promises.
- **About two thirds are exactly right** (656 of 1019, 64%). Names, versions, licence,
  `PROVIDES`, `INHERIT`, `FILE_LAYERNAME` and most variables the recipe itself sets were
  right everywhere; `SRC_URI` in 16 of 23 targets.
- **A flag means "probably wrong", not "might be slightly off".** Build paths (`S`, `B`,
  `WORKDIR`), `DEPENDS`, `OVERRIDES`, `CFLAGS` and `FILESPATH` were flagged in every target,
  and in 22 flagged values even the part sous-chef could expand was incomplete (python adds
  packages, dependencies and files). 135 correct values were flagged too. For a flagged
  value, use `var` with `authoritative: true` (or `bitbake -e`).
- **A bitbake-setup build needs no explicit machine any more** (since the w5 fixes): a tool
  call without `machine` / `distro`, and `deps` and lint, which take none, evaluate the
  fragment machine and distro; the `@default` probe equals the explicit run. When nothing
  sets `MACHINE`, the output says so ("no MACHINE is set"). Bugs 2 to 4 are fixed too (bug 2
  mattered only for sibling bbappend directories sharing a name prefix, bug 3 only for a
  `DEPENDS` without python that names a cross-toolchain virtual provider). Since then 20
  flagged values (not 22) have a literal part that differs.

Limits of this measurement: one machine, one distro, 23 evaluable targets, one release
(Wrynose); functions and variable flags were not compared; the classification follows
references one level at a time and takes the first differing one as the cause.

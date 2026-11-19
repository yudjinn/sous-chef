# Recorded outputs from the live environment

Recorded on 2026-11-17 from the environment `scripts/live-env.sh` creates (see
`docs/live-environment.md`). In every file the absolute path of the live directory is
replaced with `{{LIVE}}`, the user's home with `{{HOME}}`, the user name with `{{USER}}`,
kas's temporary `HOME` with `/tmp/tmp{{RANDOM}}` and the SSH agent socket with
`{{REDACTED}}`. Nothing else was changed.

Versions: kas 5.5, bitbake-setup 2.19.0 (PyPI), BitBake 2.18.0. Commits: yocto-kiss
`cc621c5d5725a51af9abb7b0d531cf6f9264a468` (`wrynose`), bitbake
`6e260eb6ab1631607ea414d9ff8ede1155faa5d9` (`2.18`), openembedded-core
`33a6bd6cc8a9a976902081642414bd67adc74879` (`wrynose`), meta-arm
`673e8d7c5dcd14de58092eb73e6ecc5eee5c1735` (`wrynose`); for the bitbake-setup build also
meta-yocto `b60511544c41f5d29d290681e15c9e302f747efe`, meta-openembedded
`c0fcd7cca3760ef24e087ac4a31b8f357cb204da`, meta-xilinx
`9ad7d683b1809de7c4307ac083635a9371d909f4` (all `wrynose`). Host: Arch-based (kas reports
"EndeavourOS rolling"), Python 3.14, bash 5.3.

The first samples (T9.1) were written before BitBake could start on this host and recorded
the host tools failure (`chrpath diffstat rpcgen` missing). The tools are installed now;
T9.2 replaced the two `*-hosttools.out` files with the real output of the same commands
(`kas-shell-bitbake-p.out`, `kas-shell-bitbake-getvar-T-value.out`) and added the rest.
One sample still shows the old failure: `bitbake-setup-build/bitbake-setup-init.out`, whose
last step (`bitbake-config-build enable-fragment`) hit it; the fragments were enabled
afterwards by `scripts/live-env.sh`, so that build's `conf/toolcfg.conf`, which is not
recorded here, exists now. The
`bitbake -e curl` dump (27 247 lines, 1.8 MB) is not committed; it is
`{{LIVE}}/runs/bitbake-e-curl.out`. What the samples show is in
`docs/live-verification.md`.

Commands are run from `{{LIVE}}/yocto-kiss` with `KAS=../venv/bin/kas` and
`FILES=kas/kiss.yaml:kas/dogbonedark.yaml`.

| File | Command / source |
|---|---|
| `kas-dump.yaml` | `$KAS dump $FILES` (the lock file's `overrides` come first) |
| `kas-shell-env.txt` | `$KAS shell $FILES -c 'env \| sort'` |
| `kas-shell-bitbake-p.out` | `$KAS shell $FILES -c 'bitbake -p'` (first parse, 1033 recipes, 435 skipped) |
| `kas-shell-bitbake-getvar-T-value.out` | `$KAS shell $FILES -c 'bitbake-getvar -r curl --value T'` |
| `kas-shell-bitbake-getvar-T.out` | `$KAS shell $FILES -c 'bitbake-getvar -r curl T'` (with the history) |
| `kas-shell-show-layers.out` | `$KAS shell $FILES -c 'bitbake-layers show-layers'` |
| `kas-shell-show-appends.out` | `$KAS shell $FILES -c 'bitbake-layers show-appends'` |
| `kas-shell-show-overlayed.out` | `$KAS shell $FILES -c 'bitbake-layers show-overlayed'` |
| `verify/providers-bitbake.out` | `$KAS shell $FILES -c 'bitbake-getvar --value TMPDIR; bitbake -e virtual/kernel \| grep ...; bitbake -e virtual/bootloader \| grep ...; bitbake -e curl-native \| grep ...; bitbake -e glibc \| grep ...'` (the `grep -E "^(FILE\|PN\|PV\|...)="` lines) |
| `verify/curl-fetch-dryrun.out` | `$KAS shell $FILES -c 'bitbake -n -c fetch curl'`: only `do_recipe_qa` and `do_fetch` |
| `verify/curl-fetch.out` | `$KAS shell $FILES -c 'bitbake -c fetch curl'` (succeeded) |
| `verify/curl-log.task_order` | `${T}/log.task_order` of curl after that fetch |
| `verify/two-clients-A.out`, `verify/two-clients-B.out` | two kas shells started 1.5 s apart: A `bitbake -e curl`, B `bitbake-layers show-layers`; B waits for A (see `docs/live-verification.md`) |
| `broken-fetch/meta-scratch/`, `broken-fetch/kas-scratch.yaml` | the scratch layer and kas file used for a failed fetch (they live in `{{LIVE}}/scratch/`) |
| `broken-fetch/dryrun.out` | `$KAS shell ../scratch/kas-scratch.yaml -c 'bitbake -n -c fetch broken-fetch'` |
| `broken-fetch/console.out` | `$KAS shell ../scratch/kas-scratch.yaml -c 'bitbake -c fetch broken-fetch'` (exit 1): the real console output of a failed `do_fetch` |
| `broken-fetch/log.do_fetch.1484343`, `broken-fetch/log.task_order` | that task's `${T}` files |
| `hooks/*.event.json` | `PostToolUse` events built by hand for real files and outputs of this workspace (shape of `tests/fixtures/hooks/captured/`; not captured from Claude Code) |
| `hooks/*.response.json` | what `sous-chef hook` printed for each (empty: no output), at T9.2. `bash_kas_fetch_failure.response.json` is empty because the hook did not recognise `kas shell ... -c 'bitbake ...'` then; since T9.3 it does (`live_bash_hook_digests_real_fetch_failure` checks it), and the file was not re-recorded |
| `kas-build-conf/*` | the files `kas checkout $FILES` wrote: `build/conf/*` and `build/CACHEDIR.TAG` |
| `bitbake-setup-build/bitbake-setup-init.out` | `bitbake-setup --setting default top-dir-prefix {{LIVE}} --setting default top-dir-name bitbake-builds init --non-interactive --source-overrides {{LIVE}}/bitbake-setup-sources.json {{LIVE}}/yocto-kiss/yocto-kiss.conf.json dogbonedark` (exit 1 at the `enable-fragment` step, the host tools failure, before they were installed; progress lines removed) |
| `differential/summary.txt`, `classes.tsv`, `workspace.txt` | the differential test's report (`tests/differential.rs`, docs/differential.md): per-target table and totals, every compared value with its class, and the workspace-level comparisons |
| `differential-scarthgap/`, `differential-kirkstone/` (same three files) | the same report on the release workspaces of `scripts/release-env.sh` (acc-releases, 2026-11-19; versions and commits in docs/differential.md, "Other releases") |
| `bitbake-setup-build/init-build-env` | `{{LIVE}}/bitbake-builds/kiss-wrynose-dogbonedark/build/init-build-env`, as written by bitbake-setup |
| `bitbake-setup-build/{bblayers,local}.conf`, `conf-summary.txt` | that build's `conf/` (`conf-notes.txt` and `templateconf.cfg` are empty; `site.conf` is a symlink to `../../../site.conf`) |
| `bitbake-setup-build/site.conf` | `{{LIVE}}/bitbake-builds/site.conf` |
| `bitbake-setup-build/init-build-env-environment.diff` | `diff` of `env \| sort` before and after `. init-build-env` in `env -i PATH=/usr/bin:/bin TERM=dumb LANG=en_US.UTF-8 bash` |

## y1-build: the first real build (2026-11-18)

Recorded from the first build ever run in this environment: `bitbake curl` on the kas build
(`$KAS shell $FILES -c 'bitbake curl'`, 919 tasks of 78 recipes, 28 min 31 s from an empty
`sstate-cache`, succeeded), and the experiments on curl that followed
(`docs/live-verification.md`, "The first real build"). Same versions and commits as above.
Under `y1-build/`; the same scrubbing as above.

| File | Command / source |
|---|---|
| `y1-build/pkgdata/dogbonedark/` | a copy of `build/tmp/pkgdata/dogbonedark/` after the build, **trimmed** to curl's files (`curl`, `runtime/curl*`, `runtime/libcurl*`, the `runtime-reverse/` links of curl's packaged packages, `runtime-rprovides/libcurl/`) and ca-certificates' (`runtime/ca-certificates*`, `runtime-reverse/ca-certificates`: its `FILES_INFO` holds the one escaped file name of the build). Symlinks kept; the files themselves unchanged |
| `y1-build/oe-pkgdata-util.out` | `$KAS shell $FILES -c 'set -x; bitbake-getvar --value PKGDATA_DIR; oe-pkgdata-util find-path /usr/bin/curl; ... list-pkg-files ...; lookup-recipe ...; list-pkgs -p curl; package-info curl libcurl libcurl4; ...'` (the `set -x` lines name each command; `list-pkg-files -s` is not an option and failed) |
| `y1-build/oe-pkgdata-util-find-path.out` | `oe-pkgdata-util find-path '*'` (same shell), **trimmed** to the lines of curl's and ca-certificates' packages (1062 of 10505) |
| `y1-build/oe-pkgdata-util-list-pkg-files-l-curl.out` | `oe-pkgdata-util list-pkg-files -l -p curl` |
| `y1-build/sigs/curl_%.bbappend.dict` | the scratch bbappend of the experiment (`{{LIVE}}/scratch/meta-scratch/recipes-test/curl/curl_%.bbappend`, removed afterwards) |
| `y1-build/sigs/diffsigs-curl-do_{configure,compile,install}.out` | after `bitbake curl` with that bbappend: `$KAS shell ../scratch/kas-scratch.yaml -c 'bitbake-diffsigs -c never -t curl do_configure; ... do_compile; ... do_install'`, split per command, kas's INFO lines removed (stdout and stderr together) |
| `y1-build/sigs/diffsigs-curl-do_compile-reconnect.out` | the same `do_compile` comparison as sous-chef's `sigs` captured it (the handle's content) when it ran 3 s after the previous `bitbake-diffsigs`: seven `NOTE: Reconnecting` / `Retrying server connection` lines first |
| `y1-build/log/console-configure-fail.out` | `$KAS shell ../scratch/kas-scratch.yaml -c 'bitbake curl'` with the bbappend `EXTRA_OECONF:append = " --with-libpsl"` (exit 1; kas's INFO lines removed) |
| `y1-build/log/log.do_configure.1749535`, `run.do_configure.1749535` | that task's log and run file from `${T}` |
| `y1-build/log/console-compile-fail.out` | the same command with a bbappend whose `do_compile:prepend` echoes a compiler-style error line and runs `false` (exit 1; kas's INFO lines removed) |
| `y1-build/log/log.do_compile.1757933`, `run.do_compile.1757933` | that task's log and run file |
| `y1-build/log/log.task_order` | curl's `${T}/log.task_order` after these runs (the fetch of 2026-11-17 first) |
| `y1-build/bitbake/job-build-curl.out` | the output of job 2 of the `bitbake` tool, `op=build target=curl` after `op=clean args=["cleanall"]` (init-script mode; the job's handle content) |
| `y1-build/bitbake/job-build-curl-cancel-sigterm.out` | the same after `op=cancel` 22 s in, before the fix (SIGTERM to the job's group: the client's output stops, the server built on) |
| `y1-build/bitbake/job-build-curl-cancel-sigint.out` | the same with the fix (two SIGINTs): knotty's interrupt messages and `Execution was interrupted` |
| `y1-build/bitbake/cookerdaemon-cancel-excerpt.log` | lines of `build/bitbake-cookerdaemon.log` around both cancels (the server's `Disconnecting Client` and its end 20 s later; `stateShutdown`, `stateForceShutdown`) |
| `y1-build/cve/curl_%.bbappend.vex` | the scratch bbappend for the CVE report (`inherit vex`, removed afterwards) |
| `y1-build/cve/curl_cve.json` | `build/tmp/deploy/cve/curl_cve.json` after `$KAS shell ../scratch/kas-scratch.yaml -c 'bitbake -c generate_vex curl'` |

## y3-leftovers: the leftovers of the first build (2026-11-18/19)

The same environment and versions (`docs/live-verification.md`, "y3-leftovers"). Under
`y3-leftovers/`; the same scrubbing (`{{LIVE}}`, `{{USER}}`). Unlike the y1 consoles, kas's
own INFO / ERROR lines are kept: they are part of what a kas-mode job prints.

| File | Command / source |
|---|---|
| `y3-leftovers/build1-first-build-trimmed.out` | the console of y1's first `bitbake curl` (`{{LIVE}}/runs/y1-build/logs/build1.log`), **trimmed** to lines 11-38 (from `Loading cache...` to the first tasks), 1120-1128 and 1247-1255 (around the two fetch warnings) and 2763-2772 (the end, up to `Summary: There were 3 WARNING messages.`); kas's INFO lines are not in it |
| `y3-leftovers/job-kas-build-curl-cancel.out` | the output of job 2 of the `bitbake` tool in **kas mode** (`sous-chef-kas.toml`: yocto-kiss's kas files, no `init_script`), `op=build target=curl` after `op=clean args=["cleanall"]`, `op=cancel` 22 s in, during `do_configure` (the job's handle content, whole) |
| `y3-leftovers/log.do_configure.1910658.trimmed` | that interrupted task's log, **trimmed** to its first and last four lines with a `[... lines 5-533 left out ...]` line between (the log simply stops: nothing in it records the interrupt) |
| `y3-leftovers/job-kas-build-curl.out` | the output of the next kas-mode job, `op=build target=curl wait=300` (succeeded, 39 s, one warning) |
| `y3-leftovers/curl_%.bbappend.compile`, `break-compile.patch` | the scratch bbappend and patch of the compiler error (`{{LIVE}}/scratch/meta-scratch/recipes-test/curl/`, removed afterwards) |
| `y3-leftovers/console-compile-error-trimmed.out` | `$KAS shell ../scratch/kas-scratch.yaml -c 'bitbake curl'` with them (exit 1), **trimmed** to lines 1-73 (to the first lines of `Log data follows:`) and 320-366 (from the compile lines before the error to the end); lines 74-319 are libtool compile command lines and three `make` directory lines |
| `y3-leftovers/log.do_compile.1958029.trimmed` | that task's log, **trimmed** to lines 1-3 and 250-285 |
| `y3-leftovers/run.do_compile.1958029` | that task's run file, whole |


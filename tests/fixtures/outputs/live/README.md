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
(`kas-shell-bitbake-p.out`, `kas-shell-bitbake-getvar-T-value.out`) and added the rest. The
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
| `hooks/*.response.json` | what `sous-chef hook` printed for each (empty: no output) |
| `kas-build-conf/*` | the files `kas checkout $FILES` wrote: `build/conf/*` and `build/CACHEDIR.TAG` |
| `bitbake-setup-build/bitbake-setup-init.out` | `bitbake-setup --setting default top-dir-prefix {{LIVE}} --setting default top-dir-name bitbake-builds init --non-interactive --source-overrides {{LIVE}}/bitbake-setup-sources.json {{LIVE}}/yocto-kiss/yocto-kiss.conf.json dogbonedark` (exit 1; progress lines removed) |
| `bitbake-setup-build/init-build-env` | `{{LIVE}}/bitbake-builds/kiss-wrynose-dogbonedark/build/init-build-env`, as written by bitbake-setup |
| `bitbake-setup-build/{bblayers,local}.conf`, `conf-summary.txt` | that build's `conf/` (`conf-notes.txt` and `templateconf.cfg` are empty; `site.conf` is a symlink to `../../../site.conf`) |
| `bitbake-setup-build/site.conf` | `{{LIVE}}/bitbake-builds/site.conf` |
| `bitbake-setup-build/init-build-env-environment.diff` | `diff` of `env \| sort` before and after `. init-build-env` in `env -i PATH=/usr/bin:/bin TERM=dumb LANG=en_US.UTF-8 bash` |

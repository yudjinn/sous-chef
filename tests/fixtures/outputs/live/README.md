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
"EndeavourOS rolling"), Python 3.14.

**Not recorded yet:** `bitbake -e curl`, `bitbake-layers show-layers` / `show-appends` /
`show-overlayed` and `bitbake-getvar -r curl [--value] T`. On this host every BitBake
command stops at the host tools check (`chrpath diffstat rpcgen` are not installed), so
there is no real output to record; the two `*-hosttools.out` files are what those
commands print instead. Re-run `scripts/live-env.sh --verify` once the tools are installed
and record the samples then.

Commands are run from `{{LIVE}}/yocto-kiss` with `KAS=../venv/bin/kas` and
`FILES=kas/kiss.yaml:kas/dogbonedark.yaml`.

| File | Command / source |
|---|---|
| `kas-dump.yaml` | `$KAS dump $FILES` (the lock file's `overrides` come first) |
| `kas-shell-env.txt` | `$KAS shell $FILES -c 'env \| sort'` |
| `kas-shell-bitbake-p-hosttools.out` | `$KAS shell $FILES -c 'bitbake -p'` (stdout and stderr, exit 1); `bitbake -e curl`, `bitbake-layers show-*` print the same `ERROR:` line |
| `kas-shell-bitbake-getvar-hosttools.out` | `$KAS shell $FILES -c 'bitbake-getvar -r curl --value T'` (exit 1): the same check, but ends in a Python traceback |
| `kas-build-conf/*` | the files `kas checkout $FILES` wrote: `build/conf/*` and `build/CACHEDIR.TAG` |
| `bitbake-setup-build/bitbake-setup-init.out` | `bitbake-setup --setting default top-dir-prefix {{LIVE}} --setting default top-dir-name bitbake-builds init --non-interactive --source-overrides {{LIVE}}/bitbake-setup-sources.json {{LIVE}}/yocto-kiss/yocto-kiss.conf.json dogbonedark` (exit 1; progress lines removed) |
| `bitbake-setup-build/init-build-env` | `{{LIVE}}/bitbake-builds/kiss-wrynose-dogbonedark/build/init-build-env`, as written by bitbake-setup |
| `bitbake-setup-build/{bblayers,local}.conf`, `conf-summary.txt` | that build's `conf/` (`conf-notes.txt` and `templateconf.cfg` are empty; `site.conf` is a symlink to `../../../site.conf`) |
| `bitbake-setup-build/site.conf` | `{{LIVE}}/bitbake-builds/site.conf` |
| `bitbake-setup-build/init-build-env-environment.diff` | `diff` of `env \| sort` before and after `. init-build-env` in `env -i PATH=/usr/bin:/bin TERM=dumb LANG=en_US.UTF-8 bash` |

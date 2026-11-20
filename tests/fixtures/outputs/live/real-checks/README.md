# real-checks (2026-11-20)

Recorded from the live environment (docs/live-environment.md) for the checks described in
docs/live-verification.md, "Three features never run for real". The same substitutions as
the parent README (`{{LIVE}}`, `{{HOME}}`, ...); nothing else was changed unless a file
says it was trimmed. Raw outputs: `{{LIVE}}/runs/real-checks/`.

## sigs/

The kas build of yocto-kiss, BitBake 2.18.0, after `bitbake curl` with
`zlib_%.bbappend.txt` (the scratch bbappend, `CFLAGS:append:class-target = " -O1"`) in the
scratch layer through `kas-scratch.yaml`.

| File | Command / source |
|---|---|
| `diffsigs-curl-do_configure.out`, `diffsigs-curl-do_compile.out`, `diffsigs-zlib-do_configure.out`, `diffsigs-zlib-do_compile.out` | `$KAS shell ../scratch/kas-scratch.yaml -c 'bitbake-diffsigs -c never -t <pn> <task>'`, stdout (stderr held only kas's log lines) |
| `diffsigs-curl-do_install.out` | the same for `curl do_install`, run by sous-chef's `sigs` (init-script runner) and taken from the handle it stored (stdout as printed; there was no stderr) |
| `sigdata-basehashes.out` | `{{LIVE}}/runs/real-checks/tools/sigbase.py` on the stamps directories of zlib and curl: the two newest sigdata files of each task decoded (Python `compression.zstd`), with basehash, taint and `CFLAGS` |
| `zlib_%.bbappend.txt` | the scratch bbappend (removed from the layer afterwards) |

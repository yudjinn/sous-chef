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

## ../hooks/live_*

Captured from live Claude Code sessions (2.1.328, `--model haiku`) by a capture hook and
from the `--include-hook-events` stream; scrubbed (`{{LIVE}}`, `{{HOME}}`, `{{USER}}`, also
inside Claude Code's mangled project directory name). Replayed by
`tests/hook_live_session.rs`.

| File | What |
|---|---|
| `live_bash_cut_curl_configure.event.json` | session 1's `PostToolUse` Bash event for `./bitbake curl` (a script printing the y1 console with `cat`): `stdout` cut to 29 999 bytes, `persistedOutputPath`, `persistedOutputSize` |
| `live_bash_cut_curl_configure.response_before_fix.json`, `.tool_result_before_fix.txt` | session 1: sous-chef's reply, and the tool result the model was shown |
| `live_bash_cut_curl_configure.response.json`, `.tool_result.txt` | session 3 (after the fix): the same |
| `live_edit_meta_core.event.json`, `.response.json`, `.tool_result.txt` | session 3's Edit of `meta-core/recipes-core/hello/hello_1.1.bb`: the event, sous-chef's reply, the tool result |

## multiconfig/

The Scarthgap release workspace (BitBake 2.8.1) with a temporary second configuration;
its `conf/` was restored afterwards. The differential summary of the same run is in
`../differential-scarthgap-mc/` (written by `tests/differential.rs`,
`differential_scarthgap_mc`).

| File | Command / source |
|---|---|
| `musl.conf` | the `build/conf/multiconfig/musl.conf` used |
| `local.conf.diff` | `diff` of the original `build/conf/local.conf` and the one used |
| `bitbake-e-commands.log` | `scripts/differential.sh` (`SOUS_CHEF_DIFF_WS=scarthgap-mc`): the 12 `bitbake -e` with their time and exit status |
| `bitbake-e-FILE_LAYERNAME.out` | from `bitbake -e zlib` and `bitbake -e mc:musl:zlib`: the `FILE_LAYERNAME` block and the `OVERRIDES` line |
| `bitbake-e-mc-excerpts.out` | from `bitbake -e mc:musl:zlib`: the first 16 lines of the include history and the `TCLIBC` block (`tools/mc-excerpts.sh`) |
| `bitbake-getvar.out` | ten `bitbake-getvar` calls in the workspace's environment (`tools/getvar-scarthgap.sh`), each with its command and exit status |
| `sous-chef-tools.out` | sous-chef's MCP answers (`tools/mcp.py tools/calls-mc.json`, `sous-chef-scarthgap.toml`, static), **trimmed**: the `warning: anonymous python at ...` lines removed |

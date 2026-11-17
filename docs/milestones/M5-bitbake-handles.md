# M5: BitBake backend and handles

Large output stays on disk under a handle; the agent gets a digest and slices. BitBake is run
for builds and for authoritative answers.

Read first: "Build output" in [bitbake-semantics.md](../bitbake-semantics.md), the handle
ideas in [overview.md](../overview.md), `bitbake` / `log` / `slice` in
[mcp-tools.md](../mcp-tools.md).

Parallelism: **T5.1** needs only M0 (wave 1). **T5.2** follows it. **T5.3** needs T1.5.
**T5.4** needs T5.1 and T5.3. **T5.5** needs T5.3 and T3.5. **T5.6** needs T5.1 (and T5.4 for
the job link). T5.5 and T5.6 can run at the same time.

No test in this milestone may require a real BitBake. Use a fake `bitbake` script in
`tests/fixtures/fakebb/` that prints canned output and exits with a chosen status, and
recorded real outputs under `tests/fixtures/outputs/`.

---

## T5.1 Handle store

**Depends on:** M0. **Owns:** `src/handles.rs`

- `put(kind, command, bytes) -> Handle`: id is `h:` + first 10 hex of
  sha256(kind, command, bytes); identical output gets the same handle. Stored at
  `<cache dir>/handles/<first 2 hex>/<sha256>`, with a row in the index `handles` table.
- `get(handle) -> bytes`, `meta(handle)`.
- Kinds: `log`, `env`, `build`, `value`, `text`.
- Retention: delete handles older than 14 days or beyond 500 MB total at startup
  (configurable); never delete one created in the current process.
- Refuse to store or return files matching a deny-list (`.env*`, private keys, keystores).

**Done when:** tests for stable ids, round-trip, retention, deny-list.

## T5.2 Tool: `slice`

**Depends on:** T5.1, T2.4, T2.5. **Owns:** `src/mcp/tools/slice.rs`

Per mcp-tools.md: `lines`, `grep` with `context`, `var` (for `env` handles: the history block
and final assignment of that variable), `failures` (for `log` / `build` handles: the error
blocks found by T5.6's detector; until T5.6 lands, lines matching `ERROR:`), `page`.
Each slice is capped at 8 KB and ends with what it cut and how to get the next part.
Exactly one selector per call; none means `page: 1`.

**Done when:** tests for each selector, the cap marker, an unknown handle, two selectors.

## T5.3 Environment runner

**Depends on:** T1.5, T1.3. **Owns:** `src/bitbake/env.rs`, `src/bitbake/mod.rs`

- `BbEnv::detect(cfg, ws) -> Option<BbEnv>`: needs a build directory and an init script
  (`Config.init_script`, else search for `oe-init-build-env` next to the layers). Also accept
  an already-initialised environment (`BBPATH` / `BUILDDIR` set and `bitbake` on `PATH`).
- `BbEnv::command(program, args) -> Result<tokio::process::Command>` (errors on a program
  outside the allowlist or an invalid argument): runs
  `bash -c 'source "$1" "$2" >/dev/null && shift 2 && exec "$@"' -- <init> <build_dir> <program> <args...>`.
  Arguments are passed as argv, never interpolated into the script.
- Allowlisted programs: `bitbake`, `bitbake-layers`, `bitbake-getvar`, `devtool`,
  `oe-pkgdata-util`. Validation of recipe / task / image names: `[A-Za-z0-9_.+-/]` only, and
  no leading `-`.
- Timeouts and cancellation (kill the process group).
- `status` tool gains the environment state (edit `status.rs` only to call a function
  exported here; coordinate with the T2.5 owner if it is still open).

**Done when:** tests with the fake script: detection in each mode, argv safety (a target name
containing `;` or `$(...)` is rejected and never reaches a shell), timeout kills the process.

## T5.4 Jobs and the `bitbake` tool

**Depends on:** T5.1, T5.3, T2.5. **Owns:** `src/bitbake/jobs.rs`,
`src/mcp/tools/bitbake.rs`

- Job table in `Ctx`: id, operation, target, state (`running`, `succeeded`, `failed`,
  `cancelled`), start and end time, output handle.
- Output is streamed to a file as it arrives and registered as a handle at the end; `status`
  on a running job returns the last few lines and the elapsed time.
- One `build` / `task` / `clean` job at a time; a second request returns the running job's
  id. `env` and `layers` operations may run while no build is running.
- Operations and their argv:
  - `build`: `bitbake <target>` (+ `-k` via `args: ["continue"]`)
  - `task`: `bitbake -c <task> <target>` (+ `-f` via `args: ["force"]`)
  - `env`: `bitbake -e [<target>]`, output stored as an `env` handle
  - `clean`: `bitbake -c clean|cleansstate|cleanall <target>`
  - `devtool`: `modify`, `status`, `finish`, `reset`, `build` with validated arguments
  - `layers`: `bitbake-layers show-layers | show-appends | show-recipes <pn> | show-overlayed`
- Destructive (`cleansstate`, `cleanall`, `devtool reset`, `devtool finish`): require
  `confirm: true` **and** `bitbake.allow_destructive = true` in config; otherwise return an
  error that names the setting.
- When a job finishes, its digest is produced by T5.6's digester (until merged: last 20
  lines).
- If MCP progress notifications are straightforward with `rmcp`, send them; otherwise leave
  polling via `op: status`.

**Done when:** tests with the fake script: successful and failing build, status while
running, cancel, the single-job rule, both gates on destructive operations.

## T5.5 `bitbake -e` parser and authoritative `var`

**Depends on:** T5.3, T5.1, T3.5. **Owns:** `src/bitbake/envdump.rs`,
`src/mcp/tools/var_authoritative.rs` (the hook function left by T3.5)

- First capture real `bitbake -e` output (ask the user for a sample from their build, or use
  one if a build directory is available) and store a trimmed copy under
  `tests/fixtures/outputs/`. Confirm the format described in bitbake-semantics.md and fix the
  doc.
- Parse into: variable -> final value, list of operations (kind, file, line, value), flags,
  and functions. Streaming parse; the dump can be large.
- Map absolute paths to `Loc` with `Workspace::loc_for`; paths outside the workspace are
  shown absolute.
- `var` with `authoritative: true`: run `bitbake -e <recipe>` (cache the handle per recipe,
  keyed by the index state of the recipe's tree and conf files so an unchanged tree reuses
  it), then render in the same table format as static mode, plus a line saying the source is
  BitBake and the handle id.
- Add a `compare` helper (used by M8): static vs authoritative final values for a recipe.

**Done when:** parser tests on recorded output; the `var` tool test with the fake script
returning recorded output; cache reuse test.

## T5.6 Log digests and the `log` tool

**Depends on:** T5.1, T2.5. **Owns:** `src/bitbake/logs.rs`, `src/mcp/tools/log.rs`

- Locate logs: from a job's output (`ERROR: Logfile of failure stored in: <path>`), or by
  searching the build directory for the newest `log.do_*` of a recipe and task (confirm the
  `WORKDIR` layout *verify* item; resolve through `bitbake-getvar` when the environment is
  available rather than guessing paths).
- Digester, about 600 bytes of output:
  - failing recipe, version, task, exit code;
  - the first error with a few lines of context (compiler `error:` lines, `configure: error`,
    `No rule to make target`, python tracebacks reduced to the exception and the last frame
    in a layer file, `do_fetch` failures with the URL, `do_patch` rejects with the patch
    name, QA issues `ERROR: QA Issue:`), de-duplicated;
  - for shell tasks, the failing function from `run.do_<task>` when the log names a line;
  - a pointer to the task definition in the layers (via the index) so the agent can jump to
    the recipe;
  - the handle and a hint line: `slice handle=h:... failures=true | grep=RE | lines=A-B`.
- `log` tool per mcp-tools.md.
- Export the detector for `slice failures=true` and for the Bash hook in M7.

**Done when:** tests on recorded logs for at least: compile error, configure error, fetch
failure, patch failure, QA issue, python task traceback. Each digest is under 1 KB and names
the right task and first error.

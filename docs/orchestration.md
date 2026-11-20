# Orchestration

How the build is run: the user orchestrates, launching up to five Opus agents per wave with
`claude agents`. There is no model orchestrator for now (waves 0 and 1 were run by a Fable
orchestrator session; see the hand-off notes below).

## Roles

**Orchestrator (the user).** Does not write feature code. The orchestrator:
- launches each wave's agents in parallel with `claude agents`, one agent per stream, each
  on Opus and each in its own git worktree, giving each the brief from the template below;
- reviews each hand-off against the task's *Done when* list;
- merges the agent branches into the integration branch in the order given below;
- runs the gate after every merge and fixes small integration breaks (a missed import, a
  renamed field); anything larger goes back to the agent that owns the file;
- records contract changes in `docs/architecture.md` before the next wave starts;
- keeps a short status table at the bottom of this file up to date.

Any of the review, merge or doc steps can be handed to an agent as its own task, but nothing
in this process depends on an agent doing them.

**Agents (Opus).** Each gets one stream of tasks for the wave, works only in the files
those tasks own, and ends with a hand-off. Agents are independent sessions: they do not see
each other's work or the orchestrator's context, so each brief must be complete on its own.

## Git layout

- Integration branch: `dev`. `main` is not touched; the user merges `dev` when satisfied.
- First commit on `dev`: the current skeleton plus `docs/`.
- Each agent works in its own worktree (under `.claude/worktrees/`) on a branch
  `w<wave>-<stream>` cut from `dev` at the start of the wave, and commits there. An agent's
  worktree starts on an auto-named branch, so the brief tells it to check that it starts
  from the current `dev` commit and to rename the branch. Agents do not push, merge or
  rebase.
- `dev` must contain the whole previous wave before a wave is launched, since agents branch
  from it.
- The orchestrator merges with `git merge --no-ff`, one stream at a time, gate after each.

## Gate

```
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

A wave is finished when all its streams are merged and the gate passes on `dev`. The next wave
does not start before that.

## Waves and streams

M0 is a single agent. After that, five streams per wave. Task details are in
`docs/milestones/`.

| Wave | Stream A | Stream B | Stream C | Stream D | Stream E |
|---|---|---|---|---|---|
| 0 | **M0** T0.1-T0.5 (one agent) | | | | |
| 1 | Parser T1.1, T1.2 | Workspace + config T1.3, T1.5 | Render + handles T2.4, T5.1 | MCP server T2.5 | Index schema T2.1 |
| 2 | Resolve, scanner, queries T1.4 -> T2.2 -> T2.3 | Eval core T3.1, T3.2, T3.3 | Slice + env runner T5.2, T5.3 | Scaffold engine T6.1 | Log digests T5.6 |
| 3 | Replay T3.4 | Lookup tools T2.6, T2.7 | Lint engine + tool T4.1, T4.5 | Jobs + `bitbake` tool T5.4 | Hook entry + Grep/Read T7.1, T7.2 |
| 4 | Eval tools T3.5, T3.6, T3.7 | Lint `override` rules T4.2 | Lint `org` rules T4.3 | Lint `hygiene` T4.4 + hooks T7.3, T7.4 | Scaffold recipe/append/layer/class T6.2, T6.3 |
| 5 | Authoritative `var` T5.5 | Scaffold split + placement T6.4, T6.5 | Extra views T8.3 | Plugin + docs T7.5, T8.5 | Differential + bench harness T8.1, T8.2 |

Merge order within a wave: A, B, C, D, E (earlier streams are what later ones lean on).

Notes:
- Wave 1 stream B writes a small private assignment reader for `bblayers.conf` / `layer.conf`
  because the parser lands in the same wave; wave 2 stream A switches it to `parse()`.
- T8.1 and T8.2 need a real poky build directory. Wave 5 stream E writes the harness and
  runs it only if `SOUS_CHEF_POKY` is available; otherwise the user runs it later.
- T5.5 needs a real `bitbake -e` sample. If none is available the agent builds the parser
  against the format in `bitbake-semantics.md`, leaves the *verify* marker in place, and says
  so.
- T8.4 (release diff) is optional and not scheduled.
- M9 (kas support and live verification on bootlin/yocto-kiss, `docs/milestones/M9-kas-live.md`)
  was added after wave 4. T9.1 and T9.2 should run before or beside wave 5, because T5.5
  and T8.1 need real BitBake output; T9.3 can be a sixth stream. Live runs follow the
  limits in that file: parse-only commands and fetch, unpack or patch of one small recipe;
  nothing that compiles without asking the user.
- T8.6 (workspace audit report: one large findings report over a whole build repository
  and its submodule layers) is not scheduled; it goes after wave 5, since it needs all of
  M4 and T8.3.

## Agent brief (template)

Every agent prompt contains, in this order:

1. **Stream and tasks**: e.g. "Wave 2, stream B: T3.1, T3.2, T3.3 from
   `docs/milestones/M3-evaluator.md`."
   Followed by **setup**: "Check `git log --oneline -1` shows `<dev commit>`; if not,
   `git reset --hard <dev commit>` (the worktree is fresh). Rename your branch:
   `git branch -m w<wave>-<stream>`. Work only inside your worktree."
2. **Read first**: `docs/README.md` (rules for agents), `docs/architecture.md`, the milestone
   file, and any reference doc the task names.
3. **Files you own**: copied from the tasks' *Owns* lines. "Do not edit anything else. If you
   must (a contract change, a missing dependency), make the smallest change and list it in
   your hand-off."
4. **State of the tree**: which tasks are already merged and which are being written by
   other agents right now, so the agent knows what it can call and what it must stub.
5. **Finish line**: every *Done when* item, the gate passing in the worktree, work committed
   on the worktree branch.
6. **Hand-off format** (the agent's final message):
   - branch name, final commit hash and worktree path;
   - the last lines of the gate output (test counts);
   - tasks completed, and each *Done when* item with pass / not done;
   - files touched outside the owned set, and why;
   - contract changes or additions other agents need to know;
   - BitBake behaviour verified or still unverified (with the source checked);
   - known wrong behaviour or gaps.

## Review checklist (orchestrator, per hand-off)

- Diff stays inside the owned files, apart from declared exceptions.
- No `unwrap` / `expect` / `todo!` / `unimplemented!` outside tests.
- Tests assert behaviour from the task's *Done when* list, not just that code runs.
- Tool output goes through `render.rs`; file access goes through `Workspace::confine`.
- Anything marked *verify* in `bitbake-semantics.md` that the task touched is either
  confirmed with a source or still marked.
- Claims in the hand-off match the diff and the test output.

## Status

| Wave | State | Notes |
|---|---|---|
| 0 | **done** (commit `2879e75` on `dev`, gate passes) | See "Wave 0 hand-off notes" below. |
| 1 | **done** (all five streams merged, gate passes, 170 tests) | Integrated on branch `worktree-orchestrator`; `dev` must be fast-forwarded to it before wave 2 (`git merge --ff-only worktree-orchestrator`). Paused here at the user's request; wave 2 not launched. See "Wave 1 hand-off notes" below. |
| 2 | **done** (five streams and T1.6 merged, gate passes, 418 tests) | Integrated on branch `worktree-w2-integration`; `dev` must be fast-forwarded to it before wave 3 (`git merge --ff-only worktree-w2-integration`). Wave 3 not launched. See "Wave 2 hand-off notes" below. |
| 3 | **done** (five streams plus `w3-cleanup` and `w3-docs` merged, gate passes, 548 tests) | Integrated on branch `worktree-w2-integration`. See "Wave 3 hand-off notes" below. |
| 4 | **done** (five streams plus `w4-followup` and `w4-docs` merged, gate passes, 696 tests) | Integrated on branch `worktree-w2-integration`. Wave 5 not launched. See "Wave 4 hand-off notes" below. |
| 5 | **done** (eight task branches, four fix branches and `w5-docs` merged, gate passes, 891 tests) | Includes M9 (kas support, live verification on yocto-kiss). Integrated on branch `worktree-w2-integration`. See "Wave 5 and M9 hand-off notes" below. T8.4 and T8.6 are not built. |

## Wave 0 hand-off notes (for whoever launches wave 1)

State: M0 is merged on `dev`. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
and `cargo test` pass (20 tests). Every module, the 14 tool files and the 37 lint rule files
exist as non-panicking stubs and are registered. `docs/architecture.md` was updated by the M0
agent and is the current contract; read it rather than relying on earlier summaries.

Things wave 1+ agents need to know:

- **rmcp 3.10.0** is pinned. The server does not use rmcp's tool macros: `mcp::Server`
  implements `ServerHandler::list_tools` / `call_tool` over our own `TOOLS` table, so tool
  files never import rmcp. A failing tool is returned as `Ok(CallToolResult::error(..))`.
  rmcp has no notion of deferred tools; T2.5 must decide how (or whether) to signal it.
- **Tool files**: `pub struct Args` + `pub async fn run(ctx: &Ctx, args: Args) -> Result<String>`.
  `Ctx.index` is a `tokio::sync::Mutex<Index>`.
- **`main.rs` should not need editing**: it already dispatches to `mcp::serve`,
  `mcp::index_command`, `lint::cli`, `hook::handle`.
- **Lint rule files** are named `src/lint/rules/<group>_<name>.rs` (both `/` and `-` become `_`).
- **`tests/registry.rs`** checks the tool and rule registries against `docs/mcp-tools.md` and
  `docs/lint-rules.md`; adding a tool or rule means updating the doc too.
- **Fixtures**: `tests/fixtures/README.md` states the expected answers (layer priorities,
  bbappend matches, MACHINE qemux86 / DISTRO testdistro, `virtual/foo` -> libfoo-alt, the
  misspelled override `boradx`). `syntax/` and `lint/` sit directly under `tests/fixtures/`.
  `fixture_ws()` in `tests/common/mod.rs` panics until T1.3 lands.
- `Cargo.toml` has `[lints.cargo] unused_dependencies = "allow"`; remove once `ignore` and
  `regex` are used.

Open items, not verified by M0:

- The MCP server has never run end to end (T2.5 does this).
- The CI workflow has never run on GitHub.
- Fixture README statements still marked *verify*: base vs `INHERIT` class parse order,
  bbappend application order, the value of `FILE_LAYERNAME` (and so the expected `OVERRIDES`).
- `hello.inc` uses `FILES:${PN} +=`; T4.2 must decide whether
  `override/plus-equals-with-override` ignores package overrides (it should: that form is
  idiomatic).

## Wave 1 hand-off notes (for whoever launches wave 2)

State: T1.1, T1.2, T1.3, T1.5, T2.1, T2.4, T2.5 and T5.1 are merged (branches `w1-a` ..
`w1-e`, one `--no-ff` merge each, gate after each) on the integration branch
`worktree-orchestrator`, which is `dev` plus wave 1. `dev` itself was not moved: it is
checked out in the user's main checkout, which the orchestrator session could not touch.
Fast-forward it first (`git merge --ff-only worktree-orchestrator` in the main checkout) and
cut wave 2 branches from the result. The gate passes with 170 tests.
`sous-chef serve` has now run end to end on the fixture workspace: `status` reports
`state: ready` and three layers. `Index::refresh` is still the stub, so `status` shows
`refresh failed: not implemented` and `sous-chef index` fails until T2.2.

Orchestrator changes at integration:

- `mcp::cache_dir` had its own hash of the workspace roots, different from the index's. It
  now calls `index::cache_dir`, and `Ctx::open` opens the handle store in
  `index.cache_dir().join("handles")`.
- `docs/architecture.md`: added the `handles` table (it was missing from the contract), the
  schema additions, the `Index` write API, and the additions each stream made. Read the
  "As implemented" paragraphs there before briefing wave 2.
- `docs/bitbake-semantics.md`: added the layer behaviour T1.3 checked against the BitBake
  source.

How the agents were run in wave 1 (by a Fable orchestrator session, since replaced by the
user orchestrating with `claude agents`): each one was launched with worktree isolation and
renamed its branch to `w<wave>-<stream>`. The orchestrator integrated on its own worktree
branch
(`.claude/worktrees/orchestrator`). The agent worktrees are under `.claude/worktrees/`,
which `.gitignore` does not list, so they show as untracked in the main checkout.

Things wave 2 agents need to know:

- **Stream A (T1.4 -> T2.2 -> T2.3):** replace the private `mod conf_reader` in
  `src/workspace/layers.rs` (marked TEMPORARY) with `parse()`; the `Vars` datastore logic
  around it stays. The reader ignores `BBLAYERS:append`-style assignments and
  `include` / `require` inside `bblayers.conf` and `layer.conf`. Compare against canonical
  paths (`Layer.root` is canonical). The scanner writes through `Index::write` and never
  touches `statements_fts`.
- **`status`** counts files by walking the disk; switch it to `query::layer_stats()` once
  T2.3 lands. `status.job` is always null until T5.4 gives `JobTable` a query API.
- **JSON output:** `status` serialises its own report with serde_json. Tools that render a
  table should use `Table::render_as(format, budget)`.
- **Handle retention limits** are not in `Config` / `sous-chef.toml` yet (T5.1 says
  "configurable"). Whoever next owns `src/config.rs` should add them.
- `Cargo.toml` still has `[lints.cargo] unused_dependencies = "allow"`.

Open decisions and discrepancies (not resolved in wave 1):

- **Diagnostic severity (affects T4.1).** `hygiene/parse-error` turns every parser
  diagnostic into an Error, but two diagnostics are not BitBake errors: whitespace after a
  trailing `\`, and the `addtask` quirk that concatenates repeated `before` / `after` groups.
  Making them warnings needs a severity field on `ParseDiagnostic` (a contract change).
- **Milestone and fixture README disagree with BitBake in three places.** The parser follows
  the BitBake source and adds a diagnostic in each case; `docs/milestones/M1-parser-workspace.md`
  (T1.1) and `tests/fixtures/README.md` have not been corrected:
  1. whitespace after a trailing `\` does continue the line (BitBake right-strips first);
  2. an indented comment is an "unparsed line" error in BitBake (`comments.bb` line 3);
  3. BitBake does not strip a UTF-8 BOM (`bom.bb` line 1 fails there).
- **Legacy detection:** the rule "suffix is a known machine, distro or class override" needs
  the index, so the parser does not apply it (`VAR_qemux86` is not flagged). Lint must.
- **`.conf` grammar:** `parse()` accepts `inherit`, functions and `addtask` in `.conf` files,
  which BitBake rejects.
- **Layers:** `priority`, `compat` and `depends` come from a layer's first collection only;
  `writable_layers` matches layer names, not collection names.
- Still open from wave 0: the CI workflow has never run on GitHub; the fixture README items
  marked *verify* (class parse order, bbappend order, `FILE_LAYERNAME`).

## Wave 2 hand-off notes (for whoever launches wave 3)

State: T1.4, T2.2, T2.3, T3.1, T3.2, T3.3, T5.2, T5.3, T5.6 and T6.1 are merged (branches
`w2-a` .. `w2-e`, one `--no-ff` merge each, gate after each, no conflicts) on the
integration branch `worktree-w2-integration`, which is `dev` (`88db3aa`) plus wave 2.
`dev` itself was not moved: it is checked out in the user's main checkout. Fast-forward it
first (`git merge --ff-only worktree-w2-integration` in the main checkout) and cut wave 3
branches from the result. `sous-chef index` now works on the fixture workspace.

How wave 2 was run: a Fable session launched the five agents (Opus, one worktree each) at
the user's request, reviewed each hand-off against the checklist, and integrated on its own
worktree (`.claude/worktrees/w2-integration`). The user wants implementation work done by
agents, so T1.6 (below) also went to an agent.

Decisions the user made during wave 2 (recorded in `docs/overview.md`):

- **Releases:** Wrynose (6.0 LTS) is the primary target, Scarthgap fully supported,
  Kirkstone the floor. One rule set; release-dependent rules and behaviour are gated on the
  `Feature` table in `src/release.rs`. See "Release contract" in `docs/architecture.md`.
- **Lint against the oldest claimed release:** a layer is linted against every supported
  release in its `LAYERSERIES_COMPAT`, plus the workspace's release.
- **T1.6 Release detection** was added to M1 and done at the end of wave 2.
- **T8.6 Workspace audit report** was added to M8, not scheduled (after wave 5).

Orchestrator changes at integration:

- `slice failures=true` takes its blocks from `bitbake::logs::detect` (lines containing
  `ERROR:` remain the fallback). The blocks now include the detector's context lines, and
  the `slice` test was changed to match.
- `logs::task_definition` resolves through the query layer, and the `log` tool refreshes
  the index before the lookup. Tests in `tests/w2_integration.rs`.
- `status` takes file counts from `query::layer_stats()` after a successful refresh.
- `Cargo.toml`: the `unused_dependencies = "allow"` lint setting is gone.
- `docs/architecture.md`: "As implemented in wave 2" lists what each stream added. Read it
  before briefing wave 3.

Things wave 3 briefs must say:

- **Stream A (T3.4 replay):** call `DataStore::expand_keys()` once at the end of each
  recipe, after deferred inherits. Record `export` with `mark_exported`. Functions are not
  handled by the datastore yet. `statements_by_base` with `files` orders by file, then
  position; interleaved parse order is the replay's job.
- **Stream B (T2.6, T2.7 lookup tools):** `statement_by_name` returns a `Vec`.
  `class_files` and `shadowed` take the workspace as well as the index.
- **Stream C (T4.1 lint engine, T4.5):** implement the release gating in the T4.1 text
  (`Rule::releases()`, `LintContext.releases`, `Layer::lint_releases`) and
  `[lint.severity]`. Replace `scaffold::RuleLint`'s body with a call to the engine scoped
  to the written files (a declared edit in `src/scaffold/mod.rs`), and check that findings
  from generated files show up. Still open from wave 1: parser diagnostics that are not
  BitBake errors need a severity.
- **Stream D (T5.4 jobs, `bitbake` tool):** run `validate_name` on every user-supplied
  target; `BbEnv::command` cannot tell a target from a flag. Use `Supervised` for
  cancellation. `log job=...` and `status.job` are waiting for the job table; connect both.
  `logs::failure_logfiles` and `failed_tasks` read a build's console output.
- **Stream E (T7.1, T7.2 hooks):** `bitbake::logs::detect` is the failure detector for the
  Bash hook; `query::enclosing_item` is ready.
- **Wave 4 scaffold generators (T6.2, T6.3):** each registers itself with a one-line edit
  in `scaffold::generator()`; the brief must allow it. Generated files follow `ws.release`.

T1.6 (release detection, branch `w2-t16`, merged last): `Workspace.release`,
`Layer::lint_releases`, `Config.release`, `[lint.severity]`, a `release` row in `status`.
The fixture workspace is now Wrynose; `meta-mine` claims `kirkstone scarthgap wrynose`, so
it is linted against all three. Gaps it left:

- `src/bitbake/logs.rs` decides the `PV-PR` work directory layout itself and its comments
  say "before scarthgap"; the layout changed in Nanbield. It should ask
  `Feature::PrInWorkdir` (wave 3 stream D owns the BitBake backend; add it to that brief).
- `src/index/scan.rs` and `src/index/query.rs` classify a file under `classes-global/` by
  its directory on every release; only the resolver honours `SplitClassDirs`.
- `[lint.severity]` is read but not applied or checked for unknown rule ids (T4.1).
- `docs/mcp-tools.md` does not list the `release` field of `status`.
- Some fixture recipes use pre-Styhead `WORKDIR` conventions in what is now a Wrynose
  workspace (`libfoo`, `app_0.1.bb`); no test depends on it, but lint rule fixtures in
  wave 4 will notice.
- Wrynose build environment: the 6.0 release notes mention `bitbake-setup init` and an
  `init-build-env` script that devtool looks for before `oe-init-build-env`. Where it lives
  and whether `oe-init-build-env` still works was not checked; `BbEnv` detection may need a
  third mode.

Open items and gaps (not resolved in wave 2):

- **Python in `OVERRIDES`.** OE-core's `OVERRIDES` contains python, so the static override
  list is partly unknown on real layers; the evaluator flags affected values with a
  heuristic. Check this against a real poky checkout in T8.1.
- **`validate_name` rejects `:`**, so `bitbake-getvar` cannot be asked for a name such as
  `FILES:${PN}`. T5.5 needs a decision.
- **`log`:** the `bitbake-getvar` route for finding `T` has no test (it was written against
  the `BbEnv` stub); a `TMPDIR` outside the build directory fails the workspace path check,
  so its logs cannot be read; `--value` may not exist in older `bitbake-getvar` releases.
- **Scaffold writes:** no rollback when a later file of a multi-file plan fails; the check
  before each rename is not atomic. Files of a new layer are not linted after the write.
- **Query layer:** `preferred_recipe` with a version picks the first match in layer and
  path order; `DEFAULT_PREFERENCE`, `PE` and `PR` are ignored; `BBMASK` is not applied.
- **Scanner:** directories named `tmp`, `sstate-cache` and `downloads` inside a layer are
  never indexed; non-UTF-8 paths are skipped silently.
- **Unverified BitBake behaviour:** the real `oe-init-build-env` (and whether Wrynose
  replaces it with `bitbake-setup`); the layout of `bitbake -e` output that `slice var=`
  expects; bbappend order against real `bitbake-layers show-appends` output; whether the
  BitBake server leaves the process group on a kill; the compiler, configure, make, wget
  and quilt text in the recorded logs (written by hand from the sources, not captured).
- Still open from earlier waves: the CI workflow has never run on GitHub; handle retention
  limits are not in `Config`; `tests/fixtures/README.md` does not mention `fakebb/` or
  `outputs/`; the milestone and fixture README disagreements with BitBake listed in the
  wave 1 notes.

## Wave 3 hand-off notes (for whoever launches wave 4)

State: T3.4, T2.6, T2.7, T4.1, T4.5, T5.4, T7.1 and T7.2 are merged (branches `w3-a` ..
`w3-e`, one `--no-ff` merge each, gate after each, no conflicts) on the integration branch
`worktree-w2-integration`, which was `dev` at the start of the wave (`2a4b7c6`). Two
follow-up branches were merged after the streams: `w3-cleanup` (code) and `w3-docs`
(`docs/mcp-tools.md`, `docs/lint-rules.md`, "As implemented in wave 3" in
`docs/architecture.md`). Read that section before briefing wave 4. `dev` must be
fast-forwarded again (`git merge --ff-only worktree-w2-integration` in the main checkout).

How wave 3 was run: as wave 2. The orchestrating session wrote no feature code; follow-up
work found at integration went to agents.

Resolved in wave 3:

- **Diagnostic severity and the `.conf` grammar** (open since wave 1): settled inside
  `hygiene/parse-error` by classifying parser diagnostics, with no parser contract change.
  The `addtask` quirk is a warning; whitespace after a trailing `\` is left to
  `hygiene/continuation`; statements BitBake rejects in `.conf` files are errors.
- **Fixture README *verify* items** (open since wave 0): base class before `INHERIT`
  classes, `FILE_LAYERNAME`, and bbappend order were checked in the BitBake source by T3.4.
- **Release gating in lint** and `[lint.severity]`: implemented and tested (T4.1).
- `log job=...`, the `job` line of `status`, and the work directory layout asking
  `Feature::PrInWorkdir`: connected by T5.4.

Things wave 4 briefs must say:

- **Stream A (T3.5 `var`, T3.6 `overrides`, T3.7 `deps`):** `eval::evaluate(index, ws,
  target)`; refresh the index first. `Target.recipe` takes a name, an absolute path or
  `layer:path`; `None` evaluates the configuration only. Read `DataStore::replay()`: warn
  when `anonymous` is not empty, and more strongly for an entry with `opaque: true`.
  `unset` is not in `history()`; read `unsets()` too. Functions are stored as pointer
  lines; use `DataStore::function(name)`. `recipe show=summary` still shows static values
  and says so; switching it to evaluated values is part of this stream if the brief allows
  an edit in `recipe.rs`.
- **Streams B, C, D (lint rules T4.2, T4.3, T4.4):** the `Rule` trait has `releases()` and
  `default_enabled()`. Use `lint::check_text` for fixture tests and the helpers in
  `src/lint/helpers.rs` (`finding`, `releases_lacking`, `feature_range`, `list_kind`,
  `is_package_only`, `KnownOverrides`); ask for helper additions in the hand-off instead of
  editing that file. **T4.4 must set `default_enabled` to false in
  `hygiene_variable_order.rs`**, declare the release range of `hygiene/workdir-s` with
  `feature_range(Feature::WorkdirAsSourceDir)`, and add the new rule
  `hygiene/release-syntax` (file, registry entry, row in lint-rules.md). The fixture
  workspace is Wrynose and `meta-mine` claims `kirkstone scarthgap wrynose`; some fixture
  recipes (`libfoo`, `app_0.1.bb`) use pre-Styhead `WORKDIR` conventions, which the new
  rules will report. A rule's own fixtures live under `tests/fixtures/lint/<rule-id>/`;
  **do not change the shared fixture workspace**, because other streams' snapshots depend
  on it. If a rule fires on it, report the finding in the hand-off; the snapshot of the
  `lint` tool is updated at integration.
- **Stream D also (T7.3 Bash hook, T7.4 edit hook):** fill `handle` in `src/hook/bash.rs`
  and `src/hook/edit.rs`; the trait and registry exist. The Claude Code docs confirm a hook
  may replace Bash output (`updatedToolOutput`), so a digest can be substituted.
  `bitbake::logs::detect` is the failure detector. **T7.4 needs `Index::refresh_file(ws,
  abs)`, which does not exist**: the brief must allow that addition in `src/index/`.
- **Stream E (T6.2, T6.3 generators):** register with a one-line edit in
  `scaffold::generator()`. Generated files follow `ws.release` and must lint clean through
  the real engine, which `RuleLint` now calls. Files of a new layer are not linted after
  the write, because they are not in the workspace yet.

What `w3-cleanup` did (it is not in "As implemented in wave 3", which was written at the
same time):

- New in `index::query`: `all_files`, `files_by_kind`, `files_in_layer`,
  `recipe_names_like`, `class_names_like`. No file outside `src/index/` runs SQL on the
  index any more.
- The lint engine passes `layer:path` as `Target.recipe`, so it evaluates the file being
  linted and not the preferred recipe of the same name.
- `workspace::resolve::{ClassDir, class_dir}`: on a release without `SplitClassDirs`, a
  file under `classes-recipe/` or `classes-global/` is indexed but never ranked, never an
  edge target and never part of a shadowing group.
- `eval::RefProblem::NotInRelease(Feature)`: the replay records `inherit_defer` on a
  release without it and inherits nothing. Any exhaustive `match` on `RefProblem` needs
  this arm.
- The two recipe walkers were **not** unified: `walk_recipe` in `recipe.rs` (static, used
  by `recipe`, `item`, `search`) and the replay. They agree on the fixture recipes (a test
  asserts it) and differ in the cases listed in the module doc of
  `src/mcp/tools/recipe.rs`, mainly include paths that need a variable.

Open items and gaps (not resolved in wave 3):

- **Non-writable layers and multi-release lint: decided, not yet implemented.** The user
  decided (2026-11-17, recorded in overview.md) that a layer named as the lint target, and
  every layer when none is writable, is checked against every release it claims; only an
  upstream layer swept in by `--all` gets the workspace release alone. The code still gives
  every non-writable layer the workspace release alone (`release::lint_releases`, used by
  the `ReleaseCache` in `src/lint/mod.rs`). Scheduled as an agent task at wave 4
  integration, after the three rule streams are merged, so that one agent can adjust the
  rule tests the change affects. The same task stores the release in the index so that
  changing `release` in `sous-chef.toml` rebuilds the edges.
- **Stale index after a release change.** Changing only `release` in `sous-chef.toml` does
  not rebuild edges; the index keeps edges resolved for the old release until a file
  changes. Fixing it needs the release stored in the index (a schema change).
- **`BB_DEFER_BBCLASSES`** is honoured by the replay on every release; it is absent from
  BitBake 2.8 to 2.12 and present on master. It needs a `Feature` once the first release
  that has it is checked.
- **Kirkstone:** the static walk still treats `inherit_defer` as a deferred inherit; the
  `class` tool still shows a class that exists only in `classes-recipe/` (marked as not on
  the search path).
- `--rule style` selects nothing once `hygiene/variable-order` is off by default (only the
  exact id or the alias does). The module doc of `src/bitbake/jobs.rs` says `status` calls
  `settle`; it does not. `validate_name` also rejects `%`.
- **Hook events were not captured from a live session.** The recorded events in
  `tests/fixtures/hooks/` were written by hand from the Claude Code 2.1.326 docs. The exact
  text of Grep's `content` output and whether Read's `startLine` is 1-based are unverified;
  check both against a real session before T7.5 ships the plugin.
- **Hook timing** is 2 to 8 ms per call on the fixture workspace; not measured on a real
  poky checkout. A file edited since the last refresh gets no annotation.
- **Jobs:** no MCP progress notifications (a client polls with `op: status`); one job of
  any kind at a time; a finished job's output becomes a handle only when the `bitbake` or
  `log` tool is next called; output files of a crashed server are never cleaned up;
  `devtool modify` cannot take a source tree.
- **Replay:** `addtask`, `deltask` and `addhandler` are not replayed; anonymous python
  flags only variables written with a literal `d.setVar`-family call; `BBCLASSEXTEND`
  variants and `BBFILES_DYNAMIC` are not reproduced; `LAYERDIR` is the canonical layer
  root, not the `BBLAYERS` text.
- **Lookup tools:** `class users` does not report the implicit global inheritance of
  `base` beyond a note. BitBake treats a bbappend that matches no recipe as fatal
  (`cooker.py`, `show_appends_with_no_recipes`); whether `BB_DANGLINGAPPENDS_WARNONLY`
  still exists was not confirmed, and the rule is not in bitbake-semantics.md yet.
- **Lint:** an unknown id in a suppression comment is ignored silently; the note about
  skipped upstream layers says "pass --all" in the MCP tool too, which has no such argument.
- **Unverified BitBake behaviour**, carried forward: bbappend order against real
  `bitbake-layers show-appends` output; whether the BitBake server leaves the process group
  on a cancel; whether a second client waits or fails on the lock; the Wrynose
  `bitbake-setup` environment; the `bitbake -e` layout `slice var=` expects.
- Still open from earlier waves: the CI workflow has never run on GitHub; handle retention
  limits are not in `Config`; `validate_name` rejects `:` (a T5.5 decision); python in
  `OVERRIDES` (check on a real poky checkout in T8.1).

## Wave 4 hand-off notes (for whoever launches wave 5)

State: T3.5, T3.6, T3.7, T4.2, T4.3, T4.4, T7.3, T7.4, T6.2 and T6.3 are merged (branches
`w4-a` .. `w4-e`, one `--no-ff` merge each, gate after each) on the integration branch
`worktree-w2-integration`, started from `573b9c0`. The only conflicts were in
`tests/lint_engine.rs` and `tests/lint_tool.rs`, where three rule streams changed the same
total-finding counts; the per-rule assertions were kept and the totals dropped. Two
follow-up branches were merged after the streams: `w4-followup` (code) and `w4-docs`. Read
"As implemented in wave 4" in `docs/architecture.md` before briefing wave 5.

All 38 lint rules, all 14 tools except the `inc-split` and `where` scaffold kinds and
authoritative `var`, and the three hooks now exist.

What `w4-followup` did:

- **Multi-release lint for layers that are not writable** (the user's decision): a layer
  named as the lint target or containing it, and every layer when none is writable, is
  checked against every release it claims; only a non-writable layer swept in by `--all`
  gets the workspace release alone. `release::LintReach { Target, Sweep }`.
- **The release is stored in the index** (`meta` table, `SCHEMA_VERSION` 2): changing
  `release` in `sous-chef.toml` recomputes every edge on the next refresh. This closes the
  stale-index item in the wave 3 notes.
- The edit hook's acceptance test for `override/legacy-syntax` is enabled and passes.
- Generated scaffold output was run through every real rule: no rule objected to correct
  output. A recipe generated without a license checksum or source revision gets two
  warnings, as intended; every generated recipe without a `homepage` option gets one
  info-level finding.
- `override/missing-package` no longer reports in a file whose recipes all have an empty
  `PACKAGES` (image recipes); packagegroups are still checked.
- Duplicated rule helpers moved into `src/lint/helpers.rs` and `src/index/query.rs`.

Things wave 5 briefs must say:

- **Stream A (T5.5 authoritative `var`):** `var::run` calls
  `var_authoritative::run(ctx, &args)` when `authoritative` is true; fill that file.
  `validate_name` rejects `:` and `%`, so a variable name such as `FILES:${PN}` cannot be
  passed to `bitbake-getvar` and a wildcard version cannot be a target: T5.5 must decide
  how to pass names safely. T5.5 needs a real `bitbake -e` sample; none is available, so
  the parser is built against the format in bitbake-semantics.md, the *verify* marker
  stays, and the hand-off says so. `slice var=` assumed a layout for that output which is
  also unverified; make the two agree.
- **Stream B (T6.4 `.inc` split, T6.5 placement advice):** register with a one-line edit
  in `scaffold::generator()`. `execute` refreshes the index before `plan`. Generated files
  follow `ws.release` and the releases the target layer claims; they are linted by the
  real rules after a write.
- **Stream C (T8.3 extra views):** new `show` modes in `recipe.rs` and `layers.rs`.
  `eval::providers` resolves a recipe name; `DataStore::function(name)` gives a task's
  composed pieces. `layers show=compat` overlaps `org/layer-conf`; share the checks.
- **Stream D (T7.5 plugin, T8.5 docs):** register the matchers `Grep|Read`, `Bash` and
  `Edit|Write|MultiEdit`. The Bash hook replaces output with `updatedToolOutput`; the
  others add context. **The recorded hook events were written by hand from the Claude Code
  2.1.326 docs: capture real events from a live session first** and correct the handlers if
  the shapes differ (Grep's `content` text, Read's `startLine`).
- **Stream E (T8.1 differential test, T8.2 scale check):** both need a real poky build
  directory (`SOUS_CHEF_POKY`); write the harness and run it only if one is available. The
  known places where static evaluation departs from BitBake are listed under "Open items"
  in the wave 2, 3 and 4 notes; the differential test should classify them, not rediscover
  them.

Open items and gaps (not resolved in wave 4):

- **Recipe names resolve differently across tools.** `recipe`, `var`, `overrides`, `deps`
  and lint go through provider selection (`PREFERRED_VERSION`, `DEFAULT_PREFERENCE`);
  `item`, `search`, the bbappend generator and `bitbake::logs` use
  `query::preferred_recipe` (layer priority and version only). The log locator and the
  hooks stay on the fast path on purpose; `item`, `search` and the bbappend generator
  should agree with `recipe`. Give this to a wave 5 stream.
- **The edit hook suggests `scaffold kind=where`**, which is not implemented until T6.5.
- A recipe target gives every layer it touches the full release list, including a
  non-writable layer that holds only an `.inc` the recipe includes. When the release
  changed, one edit-hook call pays for a full edge rebuild.
- **Provider resolution** does not handle `BBCLASSEXTEND` variants (`-native` and the
  like), `BBMASK`, `SKIP_RECIPE` or `COMPATIBLE_*`; a `PROVIDES` that needs expansion to
  name the item, or one set in a global class, is not found.
- **Lint on real layers:** `override/dead` drops to info whenever the override list is
  only partly known, which is the usual case with OE-core's python in `OVERRIDES`;
  `org/class-recipe-specific` and `org/class-export-functions` will flag some OE-core
  classes that do those things on purpose (visible only with `--all`);
  `org/include-must-exist` does not check a `require` whose text contains `${`;
  `override/legacy-syntax` misses `VAR_<o>` when `<o>` has no `.conf` in the workspace.
- **Hooks:** the Bash hook misses quoted inner commands (`kas shell -c "bitbake x"`); edit
  hook timing was not measured on a new file (which recomputes every edge) or on a real
  poky checkout; `Index::refresh_file` does not sync the `layers` table after a
  `layer.conf` edit.
- **Scaffold:** a new layer is not visible to a running server until it re-discovers the
  workspace; bbappend `set` covers variables only; files given by name only are not
  created; there is no `Feature` for the Whinlatter git unpack default, so git recipes use
  the same form on every release.
- **Unverified BitBake / OE-core behaviour**, added this wave: the release that dropped
  `BB_DANGLINGAPPENDS_WARNONLY`; that BitBake uses the first eligible provider when several
  remain; how `bitbake-layers add-layer` edits `bblayers.conf`; when `python_hatchling`
  first appeared; the fetcher's checksum and `SRCREV` lookup order.
- Carried forward: everything under "Open items" in the wave 3 notes that is not marked
  resolved here (hook events not captured live, jobs, replay, the Wrynose `bitbake-setup`
  environment, the CI workflow, handle retention limits, `BB_DEFER_BBCLASSES`).

## Wave 5 and M9 hand-off notes

State: every task in the plan is merged on the integration branch `worktree-w2-integration`
except T8.4 (release diff, optional) and T8.6 (workspace audit report), which were never
scheduled. The gate passes with 891 tests; the 14 live tests in `tests/live_verify.rs` pass
against the live environment. `dev` was last fast-forwarded at `2a4b7c6` (end of wave 2):
`git merge --ff-only worktree-w2-integration` in the main checkout brings it up to date.

Merged in this phase, in order: `m9-live` (T9.1), `w5-d` (T7.5), `w5-b` (T6.4, T6.5),
`w5-c` (T8.3), `m9-kas` (T9.3), `w5-a` (T5.5), `w5-e` (T8.1, T8.2), `m9-verify` (T9.2),
then four fix branches (`w5-fix-hooks`, `w5-fix-eval`, `w5-fix-lint`, `w5-fix-eval2`) and
`w5-docs` (T8.5). Two merges needed a hand resolution, both in
`docs/bitbake-semantics.md`, where several agents had added text at the same place.

How it was run: the live environment (T9.1) went first, because T5.5, T8.1 and T9.2 needed
real BitBake output; it was blocked until the user installed three host tools BitBake
requires (`chrpath`, `diffstat`, `rpcgen`). The agents that ran BitBake were each given
their own build directory (the kas build and the bitbake-setup build), because BitBake
allows one client per directory. Fix agents were not allowed to run BitBake at all: they
worked from the dumps already captured.

The live environment: `~/Development/sous-chef-live` (yocto-kiss on Wrynose, kas 5.5,
BitBake 2.18.0), created by `scripts/live-env.sh`, described in `docs/live-environment.md`.
The limits in `docs/milestones/M9-kas-live.md` held: only parse-only commands and two
fetches (`curl` and a scratch recipe) were run; nothing compiled.

What the checks against real BitBake found (details in `docs/differential.md`,
`docs/live-verification.md`, `docs/performance.md`):

- **Differential test**: 1,019 values over 23 recipes: 656 equal, 336 different but flagged
  as possibly wrong, 27 from one documented gap (the spelling of `LAYERDIR` through a
  symlink), 0 unflagged differences. A probe without an explicit machine found 12
  unflagged wrong values; after the fixes it finds none.
- **Live verification**: 18 bugs (B1 to B18), all assigned and fixed; the seven live tests
  that reproduced bugs pass.
- **Performance** on 1,784 files: first index build 1.3 s; a no-change refresh 19 ms
  (104 ms before the fix); `var` warm 29 ms; hooks 3 to 10 ms per call.
- **Lint on yocto-kiss's own layers**: one finding, which is true. With `--all`,
  `hygiene/parse-error` went from 422 findings to 3.

Decisions the user made in this phase: add kas support and a live verification milestone
(M9); use bootlin/yocto-kiss as the reference environment at
`~/Development/sous-chef-live`; no large builds (nothing that compiles without asking).

Open items, most important first (the T8.5 hand-off has the full list):

- **`override/shadowed` has 7 false positives on yocto-kiss** (found by the documentation
  pass, after the fix rounds). The rewrite of virtual providers in `DEPENDS` is recorded in
  the history as an assignment at the `PREFERRED_PROVIDER_*` line, so the rule reports a
  recipe's own `DEPENDS =` as replaced by `tcmode-default.inc`. The rule should ignore that
  history entry, or the replay should record the rewrite as its own kind of operation.
- `conf/ccache.conf` in OE-core still gives 3 `hygiene/parse-error` errors: every file
  under a layer's `conf/` counts as metadata, and this one is a data file.

- **Lint and files BitBake does not collect.** A `.bb` that no `BBFILES` glob matches is
  marked in the index and no longer treated as a recipe by the tools, but the lint engine
  still lints and evaluates it (`src/lint/mod.rs`, `layer_files`). Passing
  `FileSet::COLLECTED` there would also silence `org/recipe-path`, whose purpose is to
  report misplaced recipes; the engine needs to distinguish the two.
- **`BBFILES_DYNAMIC`** is understood by the `layers show=appends` view only. The index
  should fold a dynamic layer's globs into `BBFILES` when the named collection is present,
  so that those bbappends are evaluated and linted.
- **Not modelled**: `BBMASK`, `BBCLASSEXTEND` variants (`-native`, `nativesdk-`),
  `SKIP_RECIPE`, `COMPATIBLE_*`; `addpylib` as a statement kind; anonymous python and
  `${@...}` are never run, by design.
- **Lint rules split values with their own code**; they should use `parse::words`, which
  keeps a `${@...}` expression whole.
- **The `.inc` split** checks safety statically and refuses some safe splits; evaluating
  the result needs the evaluator to read files that are not on disk yet.
- **Hooks**: Bash and Edit events were never captured from a live Claude Code session (Grep
  and Read were); a single-file `grep -n` and compound commands are not annotated.
- **kas**: defaults from the kas files are applied before `bitbake.conf` rather than at
  `local.conf`'s place; `KAS_*` environment overrides are not read.
- **Jobs**: no MCP progress notifications; one job at a time; output of a crashed server is
  not cleaned up.
- The repository has no license file and no remote; the CI workflow has never run.
- T8.4 (release diff) and T8.6 (workspace audit report) are not built.

## After wave 5: audit report and open-item fixes

Merged on `worktree-w2-integration` after the wave 5 close-out: `w6-audit` (T8.6, the
`audit` command and MCP tool) and `w6-fix` (open items). The gate passes with 925 tests;
the 14 live verification tests and the live audit test pass against yocto-kiss. Both
agents were stopped once from Remote Control before committing anything and were
relaunched with briefs that ask for a commit after each part.

Resolved from the wave 5 open items:

- `override/shadowed` no longer reads the virtual-provider rewrite of `DEPENDS` as a
  replacement (`OpKind::VirtualProviderRewrite`); on yocto-kiss with `--all` it went from
  83 to 65 findings, all 18 removed ones being this false positive.
- A `.bb` that no `BBFILES` glob matches gets one `org/recipe-path` finding and no other
  rule; `org/layer-conf` keeps the `.bbappend` case.
- `BBFILES_DYNAMIC` is folded into the index, so dynamic-layer bbappends are evaluated and
  linted; the `layers` view reads the index.
- `conf/ccache.conf` and similar unreferenced, unparsable `.conf` files are data;
  `hygiene/parse-error` with `--all` is 0.
- Every value split in lint goes through `parse::words`.
- Notes, not models: `<pn>-native` / `nativesdk-<pn>` / multilib names are said to be
  class-extended variants; a static `BBMASK` marks files not collected; `SKIP_RECIPE` and
  `COMPATIBLE_*` add a "BitBake would skip this recipe" note when decidable.

Still open:

- **`addpylib`** is still an `Unknown` statement: making it a `StatementKind` needs one arm
  in `src/scaffold/inc_split.rs` (`StatementKind::AddPyLib { .. } => e.opaque = true`) and
  a `Feature` (it first appears in BitBake 2.4, so Kirkstone lacks it).
- **`inherit` inside a `classes/` class used through `INHERIT`** is resolved as a recipe
  inherit, so `inherit sanity` in OE-core's `sign_*.bbclass` shows as unresolved in the
  audit report (`src/index/scan.rs`).
- The audit report duplicates `org_upstream_edit`'s git helper and reads the `BBLAYERS`
  order rows through the `layers` tool's JSON, because both are private; make them public.
- The skip notes never fire on yocto-kiss (its `MACHINEOVERRIDES` holds python);
  `INCOMPATIBLE_LICENSE` is not modelled; `BBMASK` uses the default machine and distro.
- `BBFILES_DYNAMIC` set inside an included file or under an override is not followed.
- One timing test (`bitbake_kas::a_timeout_kills_the_whole_kas_process_group`) failed
  once in a full run and passed on repeat; `tests/plugin_packaging.rs` did the same in
  wave 5. Watch both.
- Carried forward unchanged: hook events not captured live for Bash and Edit; the `.inc`
  split's static safety check; kas defaults applied before `bitbake.conf`; T8.4 (release
  diff) not built; no license file, no remote, CI never run.

## Rounds 7 to 12 (closed)

Merged on `worktree-w2-integration` after the "After wave 5" notes, one or two agents per
round, gate after every merge, live tests (no BitBake) after every round:

- **w7**: `w7-diff` (T8.4 release diff: the `diff` tool and CLI) and `w7-fix` (`addpylib`
  statement kind, inherit context for `classes/`, shared git helper, typed `BBLAYERS`
  order checks, `BBFILES_DYNAMIC` through includes, two flaky tests made reliable). One
  integration fix: the diff's exhaustive match needed the `AddPyLib` arm.
- **w8**: `w8-tools` (diff on the shared git helper, old-revision `BBFILES`, typed appends
  rows, `search` kinds, process-group termination waits, stale job outputs, `KAS_*`
  overrides, every kas *verify* item settled from the kas source) and `w8-eval` (the
  `.inc` split evaluates its result through an in-memory overlay, task `depends`
  rewrite, kas defaults at `local.conf`, three-way skip and license notes, per-machine
  `BBMASK`). One integration fix: `kas_origin` uses `setting_source`.
- **w9**: `w9-final` (`.inc` inherit context in the index, shared statement rows, stale
  job outputs swept at server start, documentation pass).
- **w10**: `w10-server` (MCP progress notifications, `[handles]` retention, the job queue
  behind a read-only job) and `w10-eval` (class-extended variants evaluated,
  `PREFERRED_PROVIDERS`, `BBMASK` recomputed from its inputs).
- **w11**: `w11-final` (per-file lint progress, `features_check` and the preferred-provider
  skip, native variants through `PROVIDES`, the class-extension order on Scarthgap 5.0 to
  5.0.10, Styhead and Walnascar corrected).
- **w12**: `w12-final` (kas work directory independent of the current directory, which
  removed 3 false `org/generated-conf-edit` warnings; `nativesdk` and multilib `PROVIDES`;
  `features_check` decides only on certain words).

State at close: 1,050 tests pass (25 ignored, all live or timing tests); the 17 live tests
pass against yocto-kiss. Differential: 1,090 values compared over 25 targets, 702 equal,
357 flagged, 31 documented gap, 0 unflagged differences. Lint on yocto-kiss: 1 finding
(true) by default, 625 with `--all`, 0 errors. Skip checks agree with BitBake on 21 of 24
`show-appends` verdicts; no "would skip" names a recipe BitBake parsed.

Why the rounds stopped: what remains is either a refinement of answers that are already
flagged as uncertain, or needs something only the user can provide.

Left open, refinements (no wrong answer today; each is flagged as uncertain):
- multilib variants listed only by a recipe's own `BBCLASSEXTEND` are found by name only;
  OE-core's python-computed `MULTILIB_VARIANTS` leaves some multilib `PROVIDES` words
  flagged; `features_check` cannot give a certain pass on a flagged value even when only a
  `:remove` is uncertain;
- the three BitBake skips the notes cannot decide have a non-static `COMPATIBLE_MACHINE`
  (python is never run, by design).

Left open, needs the user:
- a license for the repository, and a git remote (which is also what lets CI run: the
  workflow has never run);
- Bash and Edit hook events captured from a live Claude Code session (Grep and Read were;
  the one attempt for the others was refused by the permission system);
- a Scarthgap workspace and a multilib configuration to check those code paths against a
  real BitBake;
- anything that needs BitBake to compile or to report its parse log (the user's rule: no
  large builds).

## After the rounds: features, real builds, CI, multi-release accuracy

Merged on `worktree-w2-integration`, in order (each with the gate re-run, and the live
tests after the ones that touch evaluation or BitBake):

- **x1 to x6** (parallel feature batch): `x1-python` (allowlist of pure python helpers,
  `src/eval/pyexpr.rs`), `x2-migrate` (`lint --as-release` / `--only-release` /
  `--migrate-to`, rule `hygiene/release-removed`, SARIF / GitHub output, baseline),
  `x3-fix` (`lint --fix`), `x4-views` (`recipe show=cve`, tools `sigs` and `pkgdata`),
  `x5-multiconfig`, `x6-benchmark` (`bench/`, docs/benchmark.md). Conflicts between
  x2/x3, x1/x5 and x4/x5 resolved by hand, keeping both sides.
- **x7-cleanup**: tool definitions 31.5 KB to 14.1 KB (schema boilerplate), skill 8.0 KB to
  2.5 KB, `alwaysLoad` on six core tools, search fixes from the benchmark, rules
  `hygiene/git-pv` and `hygiene/insane-skip`, fixes for the UNPACKDIR rules and 17 renames,
  CVE rules through `Feature`s.
- **y2-overhead**: plugin context cost measured in sessions: 3,694 tokens with tool search
  on, 5,693 off (the pilot measured 9,356 before the cuts).
- **y1-build, y3-leftovers** (the user then allowed building `curl` only): first real build
  (28.5 min, 919 tasks, 14 GB); pkgdata, CVE view and authoritative `var` confirmed against
  it; fixed: `bitbake op=cancel` (killed only the client), sigs word changes, log pointer
  for `:prepend`, hook wrapper parsing, `RRECOMMENDS` in `deps`, compiler error rows,
  cancelled-job digests, job warnings; kas mode passed the server's `TMPDIR` to BitBake.
- **z1-env**: generic variables withheld from BitBake in every runner mode (`STRIPPED_ENV`,
  only `TMPDIR`), `[bitbake] pass_env`, a `status` row, and `tests/env_names.rs` keeping
  sous-chef's own variables `SOUS_CHEF_*`.
- **ci-validate** (the repository got a remote, `yudjinn/sous-chef`): toolchain pinned to
  stable 1.99.1 (`rust-toolchain.toml`, `rust-version = "1.89"`); CI, a lint demo that
  uploads the fixture's SARIF to code scanning (category `sous-chef-fixture`), and a
  tag-triggered release workflow, all passing on GitHub on the `ci-validate` branch (draft
  PR #1); SARIF validated against the 2.1.0 schema.
- **acc-releases, acc-overflag, acc-helpers** (parallel accuracy batch): Scarthgap and
  Kirkstone differentials (workspaces in `sous-chef-live/releases/`, `bitbake -e` only;
  first runs found 2 and 31 unflagged differences, all fixed); anonymous-python writes
  proved not to happen no longer flag values (`src/eval/anon.rs`); verified class-defined
  helpers (`src/eval/classfn.rs`) and class-extension read filters in the evaluator.
  Conflict in `src/eval/replay.rs` resolved by pruning both write lists together.

State after the accuracy merge (`4f1bc1a`): 1,301 tests pass. Differential, equal / equal
but flagged / flagged (literal differs) / bug: Wrynose 969 / 34 / 31 (30) / **1**;
Scarthgap 1,100 / 40 / 39 (38) / 0; Kirkstone 1,039 / 35 / 64 (35) / 0.

The one bug (kiss-image `DEPENDS`) is a hole older than this batch, exposed by it: an
unfollowed include or inherit (`inherit_defer ${IMGCLASSES}`, python the subset cannot
evaluate) produced a note but no flag; the value had been flagged for an unrelated reason
that acc-overflag correctly removed. Lesson for later merges: a change that removes flags
can expose a missing flag elsewhere, so the merged tree's differential, not each branch's,
is the one that counts.

- **acc-unfollowed**: an unfollowed or partly followed include / inherit flags what it could
  change (with the reference named); `bitbake.conf`'s multiconfig `require` resolved
  exactly; `inherit_defer ${IMGCLASSES}` and `conf/machine-sdk/${SDKMACHINE}.conf`
  followed for their certain words, the uncertain ones read on a copy so only what they
  write is flagged (host assumption: `os.uname()` fields are one word each). Audit fixed
  four more shapes of the same hole (unindexed includes, keys `expandKeys` cannot expand,
  computed-name writes against the anon proofs, parse-time event handlers).
- **lint-dynamic-data**: files in the recipe directories of uncollected `BBFILES_DYNAMIC`
  recipes are data; `hygiene/parse-error` runs on those recipes themselves. Scarthgap
  parse-error 369 to 0, Kirkstone 365 to 0, nothing else changed.

State at `worktree-w2-integration` after both: 1,316 tests pass; all live suites pass.
Differential, equal / equal but flagged / flagged (literal differs) / bug: Wrynose 969 /
34 / 32 (30) / 0; Scarthgap 1,100 / 40 / 39 (38) / 0; Kirkstone 1,039 / 36 / 64 (35) / 0.
`lint --all`: Wrynose 649 (0 E), Scarthgap 1,176 (0 E), Kirkstone 1,098 (1 E, true).

Left open: opaque anonymous python still only warns (about 11 of 14 functions per recipe;
flagging them would flag nearly everything); `ConfigParsed` handlers neither run nor
flagged; the `lint --fix` dry run shows the first round only; sigs' dependency-hash chain
and multiconfig unverified against a real build; tests not run on macOS (only the release
build is).

## Release batch (2026-11-20)

The integration branch was pushed and opened as PR #2 into `dev` (CI green on Linux and
macOS); merging it is the user's (the permission check refuses PR merges from here). Merged
on `worktree-w2-integration` after it, each with the gate and, where evaluation changed,
all differentials on the merged tree:

- **release-prep**: CHANGELOG for 0.1.0, macOS test job (fixes: `/proc` liveness checks
  replaced by `ps`; macOS `/var` symlink in two tests), marketplace file at the repository
  root, release workflow creates a draft with changelog notes and checks the tag against
  `Cargo.toml`, `docs/releasing.md`.
- **real-checks**: `sigs` through a dependency's hash checked against a real rebuild (zlib
  `CFLAGS` via a scratch bbappend; curl and zlib built only); two fixes (a guessed "own
  inputs did not change", cause rows cut on deep chains). Multiconfig differential on
  Scarthgap (`mc:musl`, parse only): 659 values, 0 bugs. Live Claude Code session showed
  the PostToolUse Bash hook gets only the first ~30 KB plus `persistedOutputPath`; the hook
  now reads the persisted file.
- **bench-rerun**: Sonnet 5.5, 14 tasks x 2 runs, tool search on, `.gitignore` confound
  fixed for both conditions: 1.26x tokens without the plugin, 1.6-1.7x turns, tool calls
  and wall time, score 0.85 vs 0.90; $3.51. One plugin-caused wrong answer (`var` on a task
  function did not show its postfuncs), fixed in var-taskfuncs.
- **lint-precision**: stratified random sample of 150 findings over three releases, labelled
  against the source: 15 false positives, 13 fixed (not-false-positive 90% to 99%); advice
  fixed for three rules; ignored live test re-checks the labels.
- **acc-anon-models**: exact models of seven verified OE-core anonymous functions;
  literal-differs 30 / 38 / 35 to 5 / 7 / 5; native `OVERRIDES` kept uncertain (no exact
  narrowing; a config setting for the build host is the open option).
- **var-taskfuncs**: `var` on a task function lists prefuncs / postfuncs with pointers and
  the `cleandirs` / `dirs` / `lockfiles` flags; `recipe show=tasks` gains a `funcs` column.

State (`worktree-w2-integration`, CI green): 1,347 tests. Differential, equal / differ and
flagged / gap / bug: Wrynose 1,002 / 35 / 53 / 0; Scarthgap 1,139 / 44 / 3 / 0; Kirkstone
1,075 / 69 / 1 / 0; Scarthgap multiconfig 600 / 46 / 13 / 0.

The user then decided yes on all three open questions, done on the same branch:
release-profile `strip = true` (binary 20 MB to 16.8 MB); `override/remove-in-recipe` info
by default (remove-in-recipe-info; `lint --all` warnings 147 / 421 / 452 to 61 / 229 / 290);
`[build_host] arch / os` in `sous-chef.toml` (build-host): `os.uname()[4]` and
`os.uname()[0].lower()` evaluate to the configured values, unset keeps them uncertain.
With it set, differential equal / differ and flagged / gap / bug: Wrynose 1,023 / 9 / 58 /
0; Scarthgap 1,159 / 22 / 5 / 0; Kirkstone 1,097 / 47 / 1 / 0 (the multiconfig differential
was not re-run: its `musl.conf` was removed from the Scarthgap build directory after
real-checks). 1,356 tests.

Waiting on the user: merge PR #2 (closing #1 and #3, which it contains); tag v0.1.0 per
docs/releasing.md.

# Orchestration

How the build is run: one orchestrator (Fable) directing up to five Opus subagents per wave.

## Roles

**Orchestrator (Fable).** Does not write feature code. It:
- launches each wave's agents in parallel, each in its own git worktree, with `model: opus`;
- reviews each hand-off against the task's *Done when* list;
- merges the agent branches into the integration branch in the order given below;
- runs the gate after every merge and fixes small integration breaks itself (a missed
  import, a renamed field); anything larger goes back to the agent that owns the file;
- records contract changes in `docs/architecture.md` before the next wave starts;
- keeps a short status table at the bottom of this file up to date.

**Subagents (Opus).** Each gets one stream of tasks for the wave, works only in the files
those tasks own, and ends with a hand-off.

## Git layout

- Integration branch: `dev`. `main` is not touched; the user merges `dev` when satisfied.
- First commit on `dev`: the current skeleton plus `docs/`.
- Each agent works in a worktree on a branch `w<wave>-<stream>` cut from `dev` at the start of
  the wave, and commits there. Agents do not push, merge or rebase.
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

## Agent brief (template)

Every subagent prompt contains, in this order:

1. **Stream and tasks**: e.g. "Wave 2, stream B: T3.1, T3.2, T3.3 from
   `docs/milestones/M3-evaluator.md`."
2. **Read first**: `docs/README.md` (rules for agents), `docs/architecture.md`, the milestone
   file, and any reference doc the task names.
3. **Files you own**: copied from the tasks' *Owns* lines. "Do not edit anything else. If you
   must (a contract change, a missing dependency), make the smallest change and list it in
   your hand-off."
4. **State of the tree**: which tasks are already merged and which are being written by
   other agents right now, so the agent knows what it can call and what it must stub.
5. **Finish line**: every *Done when* item, the gate passing in the worktree, work committed
   on the worktree branch.
6. **Hand-off format**:
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
| 0 | not started | |
| 1 | not started | |
| 2 | not started | |
| 3 | not started | |
| 4 | not started | |
| 5 | not started | |

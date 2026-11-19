# sous-chef documentation

`sous-chef` is a stdio MCP server, written in Rust, that helps an AI agent work on Yocto /
OpenEmbedded layers. It indexes the layers and answers questions with **pointers**
(`layer:path:A-B` plus a short snippet) instead of file contents, and it models the
include/inherit chain, bbappends and the override chain correctly. What it is for, how to
install and use it: the top-level [README](../README.md). These documents are the design,
the contracts, the measurements and the project's history.

## Reading order

| Doc | What it is | Who needs it |
|---|---|---|
| [overview.md](overview.md) | Goals, decisions, scope, non-goals | Everyone, once |
| [architecture.md](architecture.md) | Module layout, crates, **shared contracts**, index schema, conventions | Everyone, before writing code |
| [bitbake-semantics.md](bitbake-semantics.md) | The BitBake behaviour we must reproduce | Parser, workspace, evaluator, lint |
| [mcp-tools.md](mcp-tools.md) | Every MCP tool: arguments and output | Anyone implementing a tool |
| [lint-rules.md](lint-rules.md) | Rule catalogue with ids | Lint tasks |
| [live-environment.md](live-environment.md) | The real Wrynose workspace (yocto-kiss, kas and bitbake-setup) sous-chef is checked against, the Scarthgap and Kirkstone parse-only workspaces, and how to use them | Anyone running the live tests |
| [live-verification.md](live-verification.md) | sous-chef side by side with BitBake on that workspace: what was confirmed, the bugs found, their status | Anyone judging how far to trust a result |
| [differential.md](differential.md) | How often static evaluation equals `bitbake -e`, and every difference | Evaluator work |
| [performance.md](performance.md) | Index, refresh, tool and hook timings on that workspace | Anyone changing the index or the hooks |
| [ci.md](ci.md) | `sous-chef lint` in CI: exit codes, SARIF and GitHub annotations, the baseline | Anyone wiring lint into a pipeline |
| [benchmark.md](benchmark.md) | Whole-task pilot: Claude Code sessions with and without the plugin, tokens and scores per task | Anyone judging what the plugin saves |
| [milestones/](milestones/) | The tasks | Whoever is assigned them |
| [orchestration.md](orchestration.md) | How the orchestrator runs five agents per wave: streams, git layout, briefs, review | The orchestrator |

## Milestones

| | Milestone | Delivers |
|---|---|---|
| M0 | [Foundation](milestones/M0-foundation.md) | Crate skeleton, shared types, stubs, fixtures. **Serial, one agent, first.** |
| M1 | [Parser and workspace](milestones/M1-parser-workspace.md) | BitBake parser, layer discovery, path resolution |
| M2 | [Index and pointer tools](milestones/M2-index-pointer-tools.md) | SQLite index, MCP server, `recipe` `item` `search` `class` `layers` `status` |
| M3 | [Evaluator](milestones/M3-evaluator.md) | Variable history, override chain, `var` `overrides` `deps` |
| M4 | [Lint](milestones/M4-lint.md) | Rule engine, rules, `lint` tool and CLI |
| M5 | [BitBake backend and handles](milestones/M5-bitbake-handles.md) | Handles, `slice`, running bitbake, `log`, authoritative `var` |
| M6 | [Scaffold](milestones/M6-scaffold.md) | `scaffold` tool |
| M7 | [Hooks and plugin](milestones/M7-hooks-plugin.md) | Claude Code hooks and packaging |
| M8 | [Verification and extras](milestones/M8-verification-extras.md) | Differential test, scale check, extra views, release diff (`diff`), workspace audit (`audit`) |
| M9 | [kas and live verification](milestones/M9-kas-live.md) | kas project support; checks against a real build environment |

## Parallel work plan

Tasks are numbered `T<milestone>.<n>`. Arrows mean "must be merged first".

```
Wave 0   M0 (all of it, one agent)
           |
Wave 1   T1.1-T1.2 parser | T1.3 layers + T1.5 config | T2.4 render | T2.5 MCP server | T5.1 handles
           |
Wave 2   T1.4 resolve | T2.1-T2.3 index | T5.2 slice | T5.3 env runner | T6.1 scaffold engine | T3.1 datastore + T3.2 expansion
           |
Wave 3   T2.6-T2.7 lookup tools | T3.3-T3.4 evaluator | T4.1 lint engine | T5.4 jobs | T5.6 logs | T7.1 hook entry
           |
Wave 4   T3.5-T3.7 eval tools | T4.2-T4.4 lint rules (3 agents) | T4.5 lint tool | T6.2-T6.5 scaffold | T7.2-T7.4 hooks
           |
Wave 5   T5.5 envdump + authoritative var | T7.5 plugin | M8
           |
M9       T9.1 live environment | T9.2 live verification | T9.3 kas; then fix rounds on the
         bugs found (eval, lint, hooks)
           |
w6-w12   T8.6 audit | T8.4 release diff | fix rounds on the open items; w9 and w12
         final passes (orchestration.md, "Rounds 7 to 12 (closed)")
```

Each task's own *Depends on* line is the authority; the waves are a suggested grouping.

Within a wave, the groups separated by `|` touch disjoint files and can run at the same time.

## Rules for agents working in parallel

1. **Stay inside the files your task owns.** Each task lists them under *Owns*. M0 creates a
   stub for every module, tool and lint rule, so you fill a file in; you do not add to a
   shared registry.
2. **Do not change `Cargo.toml`, `src/types.rs` or `src/error.rs`** without saying so in your
   hand-off. M0 adds all dependencies. If you need a new one or a contract change, make the
   smallest change and name it explicitly so other agents can rebase.
3. **Program against the contracts in [architecture.md](architecture.md)**, not against
   another agent's unfinished code. If a dependency is not merged yet, use the fixtures.
4. **Every task ends green:** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test`.
5. **Never guess BitBake behaviour.** If [bitbake-semantics.md](bitbake-semantics.md) does not
   cover a case, check the BitBake user manual or the BitBake source, then add the rule to
   that doc in the same change.
6. **Report what you did not do.** A task hand-off lists skipped acceptance items and any
   known wrong behaviour.

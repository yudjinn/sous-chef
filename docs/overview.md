# Overview

## The problem

Working on Yocto with an AI agent today means grepping and catting recipes, classes, includes
and `bitbake -e` dumps into context. That has two costs:

- **Tokens.** A recipe's behaviour is spread over the `.bb`, its `.inc` files, the classes it
  inherits, bbappends in other layers and several conf files. Reading them all is expensive,
  and `bitbake -e` for one recipe is tens of thousands of lines.
- **Correctness.** The value of a variable depends on parse order, layer priority and the
  override chain. A grep shows assignments; it does not show which one wins.

## What sous-chef does

1. **Indexes** every layer into SQLite: files, statements (variables, functions, tasks),
   include/inherit/bbappend edges, providers and dependencies.
2. **Answers with pointers.** A result is a location, `layer:path:A-B`, with a one-line
   snippet. The agent expands a pointer only when it needs the text, and gets exactly those
   lines.
3. **Models the chains.** For a recipe it replays files in BitBake's order and records every
   operation on every variable, then applies the override chain. "Where does `SRC_URI` get
   its value for this recipe on this machine" is one call.
4. **Lints** layers against Yocto conventions (override syntax, operator misuse, `.inc` and
   class organisation, layer configuration).
5. **Runs BitBake** when a build environment exists, keeps the large output on disk under a
   handle, and returns a digest.
6. **Scaffolds** recipes, bbappends, layers, classes and `.inc` splits in the right place.

## Decisions (settled, do not reopen)

| Decision | Choice |
|---|---|
| Source of truth | **Hybrid.** Own static parser and index is the core and needs no build environment. BitBake (`bitbake -e`, `bitbake-layers`) is an optional authoritative backend. |
| Scope | Read-only lookup, build log digests, scaffolding (writes files), running bitbake. |
| Best practices | Both: model the chains correctly, and lint against conventions. |
| Context features (from atlas) | Pointer results with a token budget, handles and digests, persistent SQLite index, Claude Code hooks. |
| Parser | Hand-written, statement level, lenient. `tree-sitter-bitbake` is the fallback only if this proves too costly. |
| Freshness | Stat walk (mtime + size, hash on change) on each tool call. No file watcher unless the M8 scale check shows it is needed. |
| Override syntax | Colon syntax (Yocto 3.4 "honister" and later) is the model. Legacy underscore syntax is recognised and tagged, for lint. |
| Transport | stdio only. No network at query time. |

## Ideas taken from atlas (bloodfeast/atlas)

- A derived, deletable SQLite index; deleting it loses nothing.
- Results are tab-separated tables of locations, with explicit truncation markers
  (`+N more (limit=L)`, `+N more over budget_tokens`).
- Large output is stored whole under a content-addressed handle `h:<10 hex>`; the model gets
  a digest of about 600 bytes ending with a hint line that shows how to slice it.
- Slices are capped (about 8 KB) and say how much they cut.
- Search hits carry their enclosing item, e.g. `[do_install:append 41-58]`.
- Hooks fail open: on any error the original tool result passes through unchanged.
- Freshness is silent when everything is fresh; stale files are reported in one line.

Not taken: the memory store, embeddings and semantic search, discussions, SCIP indexers.

## Non-goals

- Not a BitBake reimplementation. Python expansion (`${@...}`), anonymous python and
  `d.setVar` in classes are **not** evaluated statically. Affected values are marked
  `unevaluated-python`; authoritative mode exists for those.
- No arbitrary shell execution through MCP.
- No editing of upstream layers; writes are confined to layers marked writable.
- No GUI, no network service, no telemetry.

## Glossary

| Term | Meaning |
|---|---|
| Pointer | `layer:path:A-B`, lines 1-based and inclusive, path relative to the layer root. |
| Item | A named statement in a file: a variable assignment, a function, a task. |
| Handle | `h:<10 hex>`, a key to stored output on disk. |
| Digest | A short summary of stored output, returned in place of it. |
| Target | The thing an evaluation is for: a recipe (or global config) plus optional MACHINE and DISTRO. |
| Workspace | The build directory (if any) and the set of layers. |

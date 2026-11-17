# M7: Claude Code hooks and plugin

Makes the index useful even when the agent reaches for Grep, Read or Bash, and packages the
server so it installs in one step.

Read first: the current Claude Code documentation for **hooks** (event names, the JSON a hook
receives on stdin, what a PostToolUse hook may return, how to add context or replace output)
and for **plugins** (manifest, bundling hooks and an MCP server). Do not rely on memory or on
this document for the hook wire format; it changes between releases. Record what you
confirmed, with the Claude Code version, in `plugin/README.md`.

Parallelism: **T7.1** first. **T7.2 to T7.4** are independent handlers. **T7.5** last.

Rules for every hook:

- **Fail open.** Any error, timeout or unexpected input means: exit successfully and change
  nothing. Log the reason to the cache directory, never to stdout.
- **Fast.** Budget 150 ms per hook call. The hook opens the existing index read-only and
  refreshes only the file in question; it never does a full scan.
- **Never alter content that may be used for an Edit.** Annotations are additions.
- Hooks act only on Yocto files (`.bb`, `.bbappend`, `.inc`, `.bbclass`, `.conf`) inside the
  workspace.

---

## T7.1 Hook entry point

**Depends on:** T2.3. **Owns:** `src/hook.rs`, the `hook` branch in `src/main.rs`

- `sous-chef hook`: read the hook event JSON from stdin, dispatch on event and tool name to a
  handler, write the response JSON, exit 0.
- Handler trait and a registry with three stub handlers (files `src/hook/grep_read.rs`,
  `src/hook/bash.rs`, `src/hook/edit.rs`; convert `hook.rs` to a module directory if
  preferred and say so in the hand-off).
- Shared: workspace and index lookup from the event's working directory, the time budget,
  fail-open wrapper, a debug log.
- If Claude Code supports hooks that call a tool on a running MCP server, add a hidden `hook`
  MCP tool that calls the same handlers, to avoid starting a process per event. Otherwise
  keep the command form only.

**Done when:** tests pipe recorded event JSON for each tool into the binary and get a valid
no-op response; malformed JSON, an unknown tool and a missing index all exit 0 with no
output change.

## T7.2 Grep and Read annotation

**Depends on:** T7.1. **Owns:** `src/hook/grep_read.rs`

- Grep results: for each hit line in a Yocto file, look up `enclosing_item` and add it, e.g.
  `[do_install:append 41-58]`. If the hit is in a recipe, add one line naming the recipe and
  how many bbappends and includes affect it, with a hint to use `var` / `recipe`.
- Read results: if the read cut an item in half (range ends inside a function), add one note
  naming the item and where it ends. If a whole recipe file was read, add one line listing
  the bbappends and `require`d files that also contribute, as pointers.
- Use whichever mechanism the hook API offers for adding context without rewriting the tool
  output. Do not modify the output text itself.

**Done when:** tests with recorded Grep and Read events on fixture files assert the added
context and that original content is byte-identical.

## T7.3 Bash output digest

**Depends on:** T7.1, T5.1, T5.6. **Owns:** `src/hook/bash.rs`

- Trigger: a Bash command that ran `bitbake`, `devtool` or `bitbake-layers` and produced more
  than 3 KB.
- Store the full output under a handle; return the digest from T5.6's digester plus the slice
  hint line.
- Only replace output if the hook API supports replacing tool output; if it only supports
  adding context, add the digest and the handle and leave the output alone. State which was
  implemented.
- Small outputs and other commands pass through untouched.

**Done when:** tests with a recorded failing-build event: digest present, handle resolvable
with `slice`; a small output and a non-bitbake command are untouched.

## T7.4 Edit and Write follow-up

**Depends on:** T7.1, T4.1. **Owns:** `src/hook/edit.rs`

- After Edit / Write / MultiEdit on a Yocto file: reindex that file immediately, run
  file-scope lint rules on it, and add findings at warning level or above as context (at most
  five, then `+N more`; no findings means no output).
- If the edited file is in a layer not marked writable, add one line saying so and
  suggesting a bbappend (`scaffold kind=where`).

**Done when:** tests: an edit introducing `VAR_append` yields the `override/legacy-syntax`
finding; a clean edit yields nothing; an edit in an upstream layer yields the warning.

## T7.5 Plugin packaging

**Depends on:** T7.2, T7.3, T7.4, T2.5. **Owns:** `plugin/**`, `README.md` (install section)

- Plugin manifest, `hooks/hooks.json`, and the MCP server entry, following the current
  plugin documentation. The server command is the installed `sous-chef` binary with `serve`.
- A short skill or instruction file for the agent: when to use `recipe` / `var` / `item` /
  `search` instead of Grep and Read, how pointers and handles work, and that static results
  marked as python-dependent need `authoritative=true`.
- Top-level `README.md`: what it is, install (`cargo install --path .`), plugin install,
  manual `claude mcp add` alternative, configuration, tool list.
- Hooks can be disabled individually through `sous-chef.toml` (`[hooks] grep_read = false`,
  and so on; add the keys to `config.rs` with a note in the hand-off).

**Done when:** installing the plugin in a scratch Claude Code setup shows the server
connected and the tools listed; each hook fires on a fixture workspace (manual check,
recorded in `plugin/README.md`).

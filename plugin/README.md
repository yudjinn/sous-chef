# sous-chef Claude Code plugin

Packaging (manifest, `hooks/hooks.json`, the MCP server entry, the agent instructions) is
T7.5 and not written yet. This file records the hook wire format `sous-chef hook` is built
against (T7.1, T7.2).

## Hook wire format (confirmed)

Checked on **2026-11-17** against the documentation for **Claude Code 2.1.326** (the
installed `claude --version`, and the newest entry of the changelog that day). Pages:

- Hooks reference: <https://code.claude.com/docs/en/hooks> (markdown:
  <https://code.claude.com/docs/en/hooks.md>)
- Tool output types (the `tool_response` shapes): Agent SDK TypeScript reference,
  "Tool Output Types", <https://code.claude.com/docs/en/agent-sdk/typescript.md>
- Tools reference (Grep and Read behaviour): <https://code.claude.com/docs/en/tools-reference.md>
- Changelog: <https://code.claude.com/docs/en/changelog.md>

The format changes between releases; check these pages again when packaging (T7.5) and
when a hook stops having an effect.

### What the hook receives

A command hook gets one JSON object on stdin. Common fields: `session_id`, `prompt_id`
(absent before the first prompt), `transcript_path`, `cwd`, `scratchpad_dir` (2.1.257+),
`permission_mode`, `effort` (`{"level": ...}`), `hook_event_name`, and `agent_id` /
`agent_type` inside subagents. `PostToolUse` adds `tool_name`, `tool_input`,
`tool_response`, `tool_use_id` and an optional `duration_ms`; for an MCP tool also
`mcp_server`.

- `tool_response` in `PostToolUse` is the tool's **structured output object**, not the text
  the model sees (that is what `PostToolBatch` gets). Documented shapes used here:
  - `Read`: `{"type": "text", "file": {"filePath", "content", "numLines", "startLine",
    "totalLines", "truncatedByTokenCap"?}}`; other `type`s are `image`, `notebook`, `pdf`,
    `parts`.
  - `Grep`: `{"mode"?: "content" | "files_with_matches" | "count", "numFiles", "filenames",
    "content"?, "numLines"?, "numMatches"?, "appliedLimit"?, "appliedOffset"?, ...}`.
  - `Bash`: `{"stdout", "stderr", "interrupted", "isImage"?, ...}`.
  - `Edit`: `{"filePath", "oldString", "newString", "originalFile", "structuredPatch",
    "userModified", "replaceAll", "gitDiff"?}`.
  - `Write`: `{"type": "create" | "update", "filePath", "content", "structuredPatch",
    "originalFile", ...}`.
- `tool_input.file_path` of `Read`, `Write` and `Edit` is always absolute.
- `Grep` input: `pattern`, `path`?, `glob`?, `type`?, `output_mode`? (default
  `files_with_matches`), `-i`, `-o`, `-n`, `-A`, `-B`, `-C`, `context`, `head_limit`,
  `offset`, `multiline`.

### What the hook may return

- Exit 0 with nothing on stdout: success, no effect. This is the no-op every failure path
  of `sous-chef hook` takes. Exit 0 is the only status `sous-chef hook` uses (exit 2 on
  `PostToolUse` would show stderr to Claude as an error; other codes are non-blocking
  errors with a notice in the transcript).
- Exit 0 with one JSON object on stdout (nothing else around it): its fields apply. JSON
  that does not parse or validate is a non-blocking error, so stdout carries only the
  object.
- `hookSpecificOutput.additionalContext` (with `hookEventName: "PostToolUse"`): a string
  added to Claude's context next to the tool result, wrapped in a system reminder. **The
  tool output itself is unchanged.** This is what the Grep / Read annotation uses.
  Capped at 10,000 characters (longer text is moved to a file with a 2,000-character
  preview); `sous-chef` keeps it under 9,000. The docs advise factual statements rather
  than imperative instructions.
- `hookSpecificOutput.updatedToolOutput` replaces the output before Claude sees it, for
  built-in tools too; the value must match the tool's output shape or it is ignored.
  (`updatedMCPToolOutput` is the older MCP-only form.) Not used by T7.1 / T7.2. Relevant to
  T7.3: replacing Bash output is possible with `{stdout, stderr, interrupted, isImage}`.
- Top-level `decision: "block"` + `reason` on `PostToolUse` only adds the reason next to the
  result; not used.
- Timeouts: default 600 s for command hooks; a timed-out hook's output is discarded. The
  150 ms budget is ours, not Claude Code's.

### Matchers

`matcher` on tool events matches `tool_name`. Letters, digits, `_`, `-`, spaces, `,` and
`|` only: exact names (`Grep|Read`); anything else is an unanchored JavaScript regular
expression. T7.5 should register `Grep|Read`, `Bash`, and `Edit|Write|MultiEdit` (the
docs list `Edit` and `Write`; `MultiEdit` is not in the current tool list and is kept in
the handler only so an older client still works).

### MCP tool hooks (`type: "mcp_tool"`)

Confirmed: a hook can call a tool on a configured, connected MCP server (`server`, `tool`,
`input`; a plugin's server is named `plugin:<plugin>:<server>`). The tool's text output is
read like command-hook stdout. But `input` values are strings with `${path}` substitution
from the event JSON (for example `"${tool_input.file_path}"`); the docs do not say that a
whole object (such as `tool_response`) can be passed. A non-connected server is a
non-blocking error. **Decision for T7.1: command form only.** No hidden `hook` MCP tool was
added; see "Not verified" for what would make it worth it. `sous-chef hook` starts in a few
milliseconds (measured below), so a process per event fits the budget.

## How `sous-chef hook` uses it

- Dispatch on `hook_event_name` and `tool_name` (`src/hook.rs`, `HANDLERS`). Only
  `PostToolUse` on `Grep`, `Read` (T7.2), `Bash` (stub, T7.3) and `Edit` / `Write` /
  `MultiEdit` (stub, T7.4) have handlers; everything else is a no-op.
- The workspace is discovered from the event's `cwd` (its `sous-chef.toml`, or the global
  flags `--build-dir`, `--config`, `--layer` given in the hook command line, relative to
  `cwd`). The index must already exist (`sous-chef index`, or the MCP server, builds it);
  the hook opens it read-only and never scans.
- Debug log: `<cache root>/sous-chef/<workspace key>/hook.log` (or
  `<cache root>/sous-chef/hook.log` before a workspace is known), one line per handled call
  with the time taken and the outcome, truncated at 1 MB.
- `SOUS_CHEF_HOOK_BUDGET_MS` overrides the 150 ms budget.

## Measured cost

Fixture workspace, warm page cache, 50 calls each, whole process (start, discovery,
read-only index open, handler, exit), on the development machine, 2026-11-17:

| Event | release | debug |
|---|---|---|
| Grep, content mode (6 hits, 4 files) | 6.5 ms | 7.9 ms |
| Grep, files mode (9 files) | 6.0 ms | 7.4 ms |
| Read, cut item | 3.8 ms | 5.0 ms |
| Read, whole recipe | 3.0 ms | 5.3 ms |
| Bash (stub, no workspace lookup) | 1.8 ms | 2.6 ms |

The handler itself (as logged in `hook.log`) takes 0 to 4 ms. A real poky workspace has
more layers to discover; not measured yet (M8 scale check).

## Not verified

- The exact text of `Grep`'s `content` in `content` mode is not documented. Assumed: one
  ripgrep line per hit, `<path>:<line>:<text>` with `-n` (context lines
  `<path>-<line>-<text>`), and `<line>:<text>` when `path` is a single file. Paths are
  taken as absolute or relative to the `path` argument or `cwd`. Whether `filenames` is
  filled in `content` mode is not documented (the parser does not rely on it).
- `startLine` of `Read` is taken as the 1-based number of the first line returned.
- The recorded events under `tests/fixtures/hooks/` were written from the documented shapes,
  not captured from a live session (capturing needs a Claude Code session with a logging
  hook; see T7.5's manual check).
- Whether an `mcp_tool` hook can pass the whole `tool_response` object. If it can, a hidden
  `hook` MCP tool would save a process start per event; not needed for the budget today.

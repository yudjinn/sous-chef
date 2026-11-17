# sous-chef Claude Code plugin

This directory is a Claude Code plugin (T7.5) and a one-plugin marketplace that serves it.
It bundles the MCP server, the three `PostToolUse` hooks and a skill. Installation is in the
top-level [README](../README.md#install). This file records what the packaging rests on, the
hook wire format `sous-chef hook` is built against (T7.1 to T7.4), and what was checked
against a real Claude Code.

## Layout

| File | What it is |
|---|---|
| `.claude-plugin/plugin.json` | The manifest: name `sous-chef`, version (kept equal to `Cargo.toml`), description. No component paths: everything is in the default locations |
| `.claude-plugin/marketplace.json` | A marketplace named `sous-chef` whose one plugin has source `./`, this directory. `claude plugin marketplace add ./plugin` registers it from a clone |
| `.mcp.json` | The MCP server `sous-chef` (`plugin:sous-chef:sous-chef` in `/mcp`): `sh -c 'cd "$CLAUDE_PROJECT_DIR" 2>/dev/null; exec sous-chef serve'` |
| `hooks/hooks.json` | `PostToolUse` hooks on `Grep\|Read`, `Bash` and `Edit\|Write\|MultiEdit`, each `command -v sous-chef >/dev/null 2>&1 \|\| exit 0; exec sous-chef hook`, timeout 10 s |
| `skills/yocto-layers/SKILL.md` | The skill `sous-chef:yocto-layers`: which tool to use instead of Grep and Read, pointers and handles, static values, writing, releases, hooks, kas |

`tests/plugin_packaging.rs` keeps this honest without Claude Code: the JSON files parse and
agree with each other and with `Cargo.toml`; every command they run is a real subcommand
(`sous-chef <sub> --help`); every matcher names only tools with a handler in
`hook::HANDLERS` and every handled tool has a matcher; each hook command, run with `sh -c`
as Claude Code runs it, reaches its handler when `sous-chef` is on `PATH` and exits 0 with
no output when it is not; the MCP command, started outside the project, serves the
workspace in `$CLAUDE_PROJECT_DIR` and lists the registry's 14 tools; the events captured
from a real session give the expected context; the skill's tool names are all registered.

### Decisions

- **The binary comes from `PATH`.** A plugin can ship executables in `bin/`, but that
  directory is only put on the Bash tool's `PATH`, a source checkout has no built binary to
  ship, and claude.ai and Cowork refuse to install a plugin that has `bin/`. So the user
  installs `sous-chef` with `cargo install --path .` (into `~/.cargo/bin`), and the plugin
  runs it by name. A binary elsewhere: put it, or a symlink to it, on `PATH`.
- **Fail open when the binary is missing.** The documentation says a command hook that
  cannot start (missing executable, exit 127 from the shell) is a non-blocking error: the
  tool call goes ahead, and Claude Code shows a `hook error` notice. The hooks are written in
  shell form and test `command -v sous-chef` first, so a missing binary is a silent exit 0
  instead of a notice on every Grep, Read, Bash and Edit. Confirmed with `sh -c` in the
  tests. Without the binary the MCP server fails to connect (`claude mcp list`:
  `✘ Failed to connect — CONNECTION_CLOSED`, observed), which affects nothing else.
- **Shell form, not exec form.** Exec form (`args` set) would spawn `sous-chef` directly
  and fail to start when it is missing. Shell form runs through `sh -c` on Linux and macOS;
  on Windows it would need Git Bash, which is acceptable for a Yocto tool.
- **The server starts in the project directory.** The documentation does not say which
  working directory a plugin's stdio server gets (for a `headersHelper` of a plugin server
  it is the plugin's root), and advises servers to read `CLAUDE_PROJECT_DIR`, which Claude
  Code exports to plugin MCP processes. `sous-chef serve` finds `sous-chef.toml` and the
  build directory from its working directory, so the command changes to
  `$CLAUDE_PROJECT_DIR` first (written `$VAR`, not `${VAR}`, so the shell expands it rather
  than Claude Code's substitution). The hooks need no `cd`: `sous-chef hook` uses the
  event's `cwd`.
- **`MultiEdit` is kept in the matcher.** The current tools reference lists `Edit`,
  `Write` and `NotebookEdit`, not `MultiEdit`. A matcher of letters and `|` compares exact
  names, so the extra name never matches on a current client and costs nothing, and the edit
  handler still handles it for an older client.
- **Every hook is `PostToolUse`.** The Grep / Read and edit hooks, and the grep-through-Bash
  annotation, add `additionalContext`; the Bash digest replaces the output of a large
  bitbake command with `updatedToolOutput` (see below).
- **`[hooks]` switches.** `sous-chef.toml` may set `[hooks] grep_read`, `bash` and `edit`
  (booleans, default `true`). Each handler names its switch (`Handler::switch`, by default
  its `name()`); a switched-off handler is not run and, with no other handler for the
  tool, the call exits 0 with no output. The grep-through-Bash annotation (handler
  `bash_grep`) follows **`grep_read`**, because it is the Grep annotation; the BitBake
  digest (handler `bash`) follows `bash`. A `sous-chef.toml` that does not load (an
  unknown key, say) switches nothing off; the handlers then fail open on it.
- **No `license` in the manifest.** The repository has no license yet; `claude plugin
  validate` passes, but its directory-submission check reports `LICENSE_MISSING`. Set
  `license` (or add a `LICENSE`) when one is chosen. No icon either (a directory warning
  only).

### Plugin format (confirmed)

Checked on **2026-11-17** against the documentation for **Claude Code 2.1.326** (the
installed `claude --version`). Pages (each also as `.md`):

- Plugins overview: <https://code.claude.com/docs/en/plugins>
- Manifest reference: <https://code.claude.com/docs/en/plugins-reference> (fields, path
  rules, standard layout, `${CLAUDE_PLUGIN_ROOT}` and the other variables)
- Components: <https://code.claude.com/docs/en/plugins/components> (hooks, MCP servers,
  skills, `bin/`)
- Marketplace reference: <https://code.claude.com/docs/en/plugins/marketplace-reference>
- Installing: <https://code.claude.com/docs/en/plugins/install>; loading:
  <https://code.claude.com/docs/en/plugins/loading>
- MCP, "Plugin-provided MCP servers": <https://code.claude.com/docs/en/mcp>
- Hooks, command hook fields, exec and shell form, failure handling:
  <https://code.claude.com/docs/en/hooks>
- Skills, frontmatter: <https://code.claude.com/docs/en/skills>
- Tools reference (tool names; Grep and Glob absent by default on Linux and macOS):
  <https://code.claude.com/docs/en/tools-reference>

What the layout rests on:

- The manifest is `.claude-plugin/plugin.json`; only `name` is required. Every other file
  sits at the plugin root. Default locations: `hooks/hooks.json` (the event map under a
  top-level `"hooks"` key, the shape of `hooks` in `settings.json`), `.mcp.json` (an
  `mcpServers` map), `skills/<name>/SKILL.md`. Plugin hooks fire from session start, not
  only when a skill is used.
- A plugin MCP server is named `plugin:<plugin>:<server>`; its tools are
  `mcp__plugin_<plugin>_<server>__<tool>` (here `mcp__plugin_sous-chef_sous-chef__var`, and
  so on), the names for permission rules and hook matchers.
- `CLAUDE_PLUGIN_ROOT`, `CLAUDE_PLUGIN_DATA` and `CLAUDE_PROJECT_DIR` are exported to hook
  and stdio MCP processes; `${...}` references are substituted in hook `command` / `args`
  and MCP `command` / `args` / `env`.
- A skill's `SKILL.md` has YAML frontmatter; `description` is the field Claude uses to
  decide when to load it (description plus `when_to_use` truncated at 1,536 characters in the
  listing); once loaded, the body stays in context.
- A marketplace is `.claude-plugin/marketplace.json` (`name`, `owner`, `plugins`); a
  relative plugin `source` resolves from the marketplace root, and `"./"` names the root
  itself. A marketplace added from a local path is read in place, so a `git pull` updates
  the plugin.
- `claude plugin validate ./plugin` (and `--strict`) reports `✔ Validation passed` for this
  directory, listing the three hooks, the `sous-chef` stdio server and the skill
  `sous-chef:yocto-layers`.

### Checked with Claude Code 2.1.326 (2026-11-17)

On a copy of `tests/fixtures/ws`, indexed with `sous-chef index`, `sous-chef` (release
build) on `PATH`:

- `claude -p ... --plugin-dir ./plugin --tools Read,Grep` (model haiku): the debug log
  shows the plugin loaded with its hooks and one skill, `MCP server
  "plugin:sous-chef:sous-chef": Successfully connected (transport: stdio)` with tools and
  resources, the `status` tool called and answering `release wrynose
  (LAYERSERIES_CORENAMES)` for the project directory, and the plugin hook running on one
  Grep and two Reads (`finished with status 0`, 5 to 7 ms, `provided additionalContext`);
  `hook.log` has `grep_read 1 ms: context added` for each.
- In a scratch `CLAUDE_CONFIG_DIR`: `claude plugin marketplace add ./plugin` (`Successfully
  added marketplace: sous-chef`), `claude plugin install sous-chef@sous-chef`
  (`Successfully installed`, read in place from `plugin/`), `claude mcp list` in the
  workspace: `plugin:sous-chef:sous-chef: ... ✔ Connected`; with `sous-chef` off `PATH`:
  `✘ Failed to connect`.
- **Not checked live:** the Bash and edit hooks. A second run that would have edited a file
  in the scratch copy and run a fake `bitbake` script there was refused by the permission
  classifier of the session doing the check, and was not retried another way. Both are
  covered by `tests/plugin_packaging.rs` through the plugin's own hook command, and by the
  handler tests, against the hand-written events.
- The server's start-up line on stderr (`sous-chef: serving 14 tools on stdio for 3
  layers`) appears in Claude Code's debug log at `[ERROR]` level: Claude Code logs all server
  stderr that way. Harmless.

### Real events

Captured in the run above by a project hook that appended its stdin to a file; stored in
`tests/fixtures/hooks/captured/` (scratch path replaced by `{{WS}}`). Differences from the
hand-written events in `tests/fixtures/hooks/`:

- **Grep, `content` mode** (`path: "."`, `-n: true`): `content` is one line per hit,
  `<path>:<line>:<text>`, as assumed. The paths are **relative to the session's `cwd`**
  when `path` is relative (`meta-core/recipes-core/hello/hello.inc:16:\tinstall -d ...`); the
  hand-written event has absolute paths. Hits come in ripgrep's order, not sorted.
  `numFiles` is **0** in content mode (the hand-written event has 4) and `filenames` is
  empty (as assumed); there is an extra field `totalLines` (equal to `numLines`). Context
  lines (`-A`/`-B`/`-C`) were not captured, so their format is still unverified.
- **Read, `startLine`**: with `offset: 10` the content starts with line 10 and
  `startLine` is 10: **1-based, confirmed**.
- **Read, `file.filePath`** is **relative to `cwd`** (`meta-core/recipes-core/hello/hello.inc`)
  while `tool_input.file_path` is absolute. The hand-written events have an absolute
  `filePath`. The handler takes `filePath` and resolves a relative one against the event's
  `cwd` (`Event::abs_path`), so it worked; it would not if the session's `cwd` changed
  between the tool call and the hook (not seen).
- **Read of a file that ends in a newline**: `content` keeps the final `\n`, and `numLines`
  and `totalLines` count the empty line after it (10 for the 9-line `hello_1.1.bb`, 21 for
  the 20-line `hello.inc`). The hand-written events have no final newline and count 9 and
  20. Effect seen in the first run: the whole-recipe note said `read of
  ...hello_1.1.bb:1-10`, one line past the end. Fixed: the handler now takes the number of
  lines from `content` and clamps the range to the lines of the file on disk (`numLines`
  and `totalLines` are not trusted); the captured events are in `tests/hook_grep_read.rs`.
- Common fields: the same set as the hand-written events (`session_id`, `prompt_id`,
  `transcript_path`, `cwd`, `permission_mode`, `effort`, `hook_event_name`, `tool_name`,
  `tool_input`, `tool_response`, `tool_use_id`, `duration_ms`); no `scratchpad_dir` in this
  `-p` session.

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
  (`updatedMCPToolOutput` is the older MCP-only form.) Used by the Bash hook (T7.3): it
  replaces the output of a large `bitbake`, `devtool` or `bitbake-layers` command with a
  digest, as `{stdout, stderr, interrupted, isImage}` with the other fields kept, after
  storing the full output under a handle. The Grep / Read hook (T7.2) and the edit hook
  (T7.4) use `additionalContext` only and never replace the output.
- Top-level `decision: "block"` + `reason` on `PostToolUse` only adds the reason next to the
  result; not used.
- Timeouts: default 600 s for command hooks; a timed-out hook's output is discarded. The
  150 ms budget is ours, not Claude Code's.

### Matchers

`matcher` on tool events matches `tool_name`. Letters, digits, `_`, `-`, spaces, `,` and
`|` only: exact names (`Grep|Read`); anything else is an unanchored JavaScript regular
expression. T7.5 registers `Grep|Read`, `Bash`, and `Edit|Write|MultiEdit` (the docs list
`Edit` and `Write`; `MultiEdit` is not in the current tool list and is kept only so an
older client still works).

**Grep is not in the default tool set on Linux, macOS and WSL** (tools reference, checked
for 2.1.326): Claude searches with `grep` and `find` through the Bash tool (embedded `ugrep`
and `bfs`), and those calls reach hooks as `Bash`. Grep comes back when `--tools` or
`--allowedTools` names it, when Bash is removed, or in a subagent that lists Grep without
Bash. So in a default session the `Grep|Read` hook fires for Read only. To cover that, the
`Bash` hook also annotates a search run through Bash (`src/hook/bash_grep.rs`): one simple
command (no pipeline, list, redirection other than a trailing `2>/dev/null`, substitution
or assignment) running `grep` or `rg` with `-n` / `--line-number` and only options that
keep the `<path>:<line>:<text>` shape. Its `stdout` is read with the Grep handler's parser
and annotated by the same code (`grep_read::annotate_hits`), as `additionalContext`, for at
most 200 hits (the context says when the cap was reached); the output is not changed.
`rg` without `-n` is left alone: ripgrep 15.2.0's `--help` says line numbers are on by
default only when stdout is a tty and off otherwise, so its output in the Bash tool would be
`<path>:<text>`. A one-file `grep -n` prints `<line>:<text>` (GNU grep 3.12 and ugrep 7.8.4:
the file name is shown by default only when more than one file is searched) and is skipped:
the handler does not guess the file. The Bash events used in the tests are hand-written in
the documented shape, with the real output of GNU grep 3.12 and ripgrep 15.2.0 on the
fixture; no Bash event was captured live. The shell snapshot of a Claude Code 2.1.326 Bash
session defines `grep` as a function that runs the embedded ugrep (`ARGV0=ugrep <claude
binary> -G --ignore-files --hidden -I ...`, falling back to the system `grep` for some
options such as `-Z` and `--null`) and defines no `rg`; ugrep 7.8.4 (`grep --version` in
that shell) prints the same `<path>:<line>:<text>` and `<path>-<line>-<text>` forms
(checked on the fixture).

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
  `PostToolUse` on `Grep`, `Read` (T7.2), `Bash` (T7.3: the digest, then the grep
  annotation) and `Edit` / `Write` / `MultiEdit` (T7.4) have handlers; everything else is
  a no-op. Several handlers may take one tool: the switched-on ones are tried in order
  until one returns a response or an error.
- Grep `content` lines: a line is a hit only when exactly one of its
  `<prefix>:<n>:` / `<prefix>-<n>-` splits names an existing Yocto file (a relative path
  tried against `cwd` and the search directory) and that split is a match (`:`). Context
  lines, lines no split explains and lines two splits explain are skipped, never
  attributed elsewhere. `numFiles` is never read (it is 0 in content mode), nor
  `filenames` in content mode.
- Read ranges: the number of lines comes from `content`, and the range is clamped to the
  lines of the file on disk; `numLines` and `totalLines` (one too many for a file that ends
  in a newline) are not trusted.
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
| Bash `grep -rn`, 400 hit lines (200 looked at, 5 files) | 10.1 ms | 28.0 ms |

The last row was measured later (same machine, 2026-11-17, median of 50 whole-process
runs, after the grep-through-Bash handler was added); the handler itself logged 24 ms in
the debug build. `tests/hook_bash_grep.rs` runs it against the 150 ms budget.

The handler itself (as logged in `hook.log`) takes 0 to 4 ms. A real poky workspace has
more layers to discover; not measured yet (M8 scale check).

## Not verified

- The format of Grep's context lines (`-A`, `-B`, `-C`) in `content` mode was not
  captured. The handler follows ripgrep's documented `--field-context-separator` (`-`,
  `<path>-<line>-<text>`, confirmed with ripgrep 15.2.0) and skips such lines; whatever
  their real form, a line that is not certainly a hit is skipped, not misattributed.
  `<line>:<text>` when `path` is a single file was not captured either (ripgrep's `--help`:
  no file name is the default when it is told to search one file); the handler reads both
  that and `<path>:<line>:<text>` for a one-file search. Hit lines (`<path>:<line>:<text>`,
  paths relative to `cwd` for `path: "."`) and the empty `filenames` were confirmed by the
  captured event (see "Real events"); for another relative `path`, whether paths are
  relative to `cwd` or to it was not captured, so both are tried and an ambiguous line is
  skipped.
- Whether Claude Code's Bash tool runs commands with a tty. Assumed not (then `rg` prints
  no line numbers unless given `-n`, and no headings). With a tty, `rg -n` would print
  headings and `<line>:<text>`, which the Bash grep handler skips.
- The Bash and Edit / Write events have not been captured from a live session; their
  recorded events under `tests/fixtures/hooks/` are still the hand-written ones. Read's
  `startLine` is now confirmed 1-based.
- Whether an `mcp_tool` hook can pass the whole `tool_response` object. If it can, a hidden
  `hook` MCP tool would save a process start per event; not needed for the budget today.

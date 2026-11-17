# sous-chef

`sous-chef` is a stdio MCP server, written in Rust, that helps an AI agent work on Yocto /
OpenEmbedded layers. It indexes every layer of a workspace into SQLite and answers with
**pointers** (`layer:path:A-B` plus a one-line snippet) instead of file contents. It replays
a recipe's files in BitBake's order, with its includes, classes, bbappends and the override
chain, so "where does `SRC_URI` get its value for this recipe on this machine" is one call.
It also lints layers against Yocto conventions, scaffolds recipes, bbappends, layers and
classes, and runs BitBake when a build environment exists, keeping large output on disk
under a handle. Three Claude Code hooks make the index useful even when the agent reaches
for Grep, Read or Bash.

## Install

### The binary

Needs a recent stable Rust toolchain (the crate uses edition 2024). SQLite is bundled.

```sh
cargo install --path .        # installs sous-chef into ~/.cargo/bin
sous-chef --version
```

The plugin and the MCP configurations below run `sous-chef` by name, so `~/.cargo/bin`
(or wherever the binary is) must be on `PATH` for Claude Code.

### The Claude Code plugin

The plugin in [`plugin/`](plugin/) bundles the MCP server, the hooks and a skill for
working on layers. It is also a one-plugin marketplace, so it installs from a clone:

```sh
claude plugin marketplace add ./plugin
claude plugin install sous-chef@sous-chef
```

(Inside a session: `/plugin marketplace add ./plugin`, then `/plugin install
sous-chef@sous-chef`.) The plugin is read in place from `plugin/`, so a `git pull` updates
it. To try it for one session without installing: `claude --plugin-dir ./plugin`.

Start Claude Code in the project directory (the one with `sous-chef.toml`, or the build
directory's parent). `/mcp` should show `plugin:sous-chef:sous-chef` connected; its tools
are `mcp__plugin_sous-chef_sous-chef__<tool>`. Details, and what was checked against which
Claude Code version, are in [plugin/README.md](plugin/README.md).

### Without the plugin

Any MCP client can run the server over stdio. The server finds the workspace from its
working directory, so give it the configuration explicitly when the client starts it
elsewhere:

```sh
claude mcp add sous-chef -- sous-chef serve --config /path/to/project/sous-chef.toml
```

or, as JSON for a client's MCP configuration:

```json
{
  "mcpServers": {
    "sous-chef": {
      "command": "sous-chef",
      "args": ["serve", "--build-dir", "/path/to/project/build"]
    }
  }
}
```

The hooks are optional; to use them without the plugin, copy the `PostToolUse` entries of
[`plugin/hooks/hooks.json`](plugin/hooks/hooks.json) into `.claude/settings.json`.

## Configuration

`sous-chef.toml` in the working directory is optional (or give `--config FILE`). Without
it, sous-chef walks up from the working directory to the first `conf/bblayers.conf`, and
failing that treats directories with a `conf/layer.conf` as the layers. Paths are relative
to the file. A minimal example:

```toml
build_dir = "build"                   # where conf/bblayers.conf lives
writable_layers = ["meta-mycompany"]  # scaffold writes only here; lint checks these by default
release = "scarthgap"                 # optional; detected from the core layer otherwise
```

Other keys: `layers` (extra layer roots, for a workspace without a build directory),
`init_script` (sourced before any bitbake command), `[bitbake] enabled` and
`allow_destructive`, `[lint] disable` and `[lint.severity]`, `[output] budget_tokens`,
`[hooks] grep_read`, `bash` and `edit` (see [Hooks](#hooks)). See "Configuration" in
[docs/architecture.md](docs/architecture.md).

## A short tour

Output from the fixture workspace in `tests/fixtures/ws` (copied from the snapshots in
`tests/snapshots/`).

`recipe` with `name: "hello"`: the recipe, its versions and bbappends in application order,
and key values with where they were set.

```
field	value	loc
recipe	hello 1.1	meta-core:recipes-core/hello/hello_1.1.bb
layer	meta-core (priority 5)	-
version	1.1 (preferred by layer priority and version)	meta-core:recipes-core/hello/hello_1.1.bb
version	1.0	meta-core:recipes-core/hello/hello_1.0.bb
bbappend	meta-vendor (priority 6)	meta-vendor:recipes-core/hello/hello_%.bbappend
bbappend	meta-mine (priority 10)	meta-mine:recipes-core/hello/hello_1.1.bbappend
inherit	autotoolsish	meta-core:recipes-core/hello/hello.inc:11
SUMMARY	GNU Hello stand-in	meta-core:recipes-core/hello/hello.inc:1
LICENSE	MIT	meta-core:recipes-core/hello/hello.inc:5
SRC_URI	4 entries from 4 applied operations; 1 inactive operation (override not active)	-
DEPENDS	3 entries from 2 applied operations	-
note: key values are evaluated for MACHINE=qemux86 DISTRO=testdistro (static replay of the configuration, classes, recipe and bbappends; python not run); use var for the full history
```

`var` with `name: "EXTRA_OECONF"`, `recipe: "hello"`, `machine: "boardx"`: every operation
in parse order, which ones apply, and the final value. The misspelt `boradx` override is
`inactive`.

```
status	op	override	loc	snippet
shadowed	?=	-	meta-core:conf/bitbake.conf:42	EXTRA_OECONF ?= ""
applied	=	append:pn-hello	build:conf/local.conf:9	EXTRA_OECONF:append:pn-hello = " --enable-local-debug"
shadowed	?=	-	meta-core:classes-recipe/autotoolsish.bbclass:7	EXTRA_OECONF ?= ""
applied	=	-	meta-core:recipes-core/hello/hello.inc:13	EXTRA_OECONF = "--disable-nls"
applied	=	append:boardx	meta-vendor:recipes-core/hello/hello_%.bbappend:7	EXTRA_OECONF:append:boardx = " --enable-boardx"
inactive	=	append:boradx	meta-vendor:recipes-core/hello/hello_%.bbappend:9	EXTRA_OECONF:append:boradx = " --enable-typo"
applied	=	append	meta-mine:recipes-core/hello/hello_1.1.bbappend:6	EXTRA_OECONF:append = " --with-mine"
final: "--disable-nls --enable-local-debug --enable-boardx --with-mine"
```

`item` with `loc: "meta-mine:recipes-app/app/app_0.1.bb:25-29"`: the exact lines, usable as
an Edit string, with the hash of the file as read.

```
meta-mine:recipes-app/app/app_0.1.bb:25-29 sha256=8c263137e2cb
fakeroot do_install() {
	install -d ${D}${bindir}
	install -m 0755 app ${D}${bindir}/app
	chown root:root ${D}${bindir}/app
}
```

## Tools

| Tool | What it answers |
|---|---|
| `recipe` | A recipe's files, versions, bbappends, classes and key values; its file tree; its functions and tasks |
| `var` | The history and final value of a variable for a recipe, machine and distro |
| `overrides` | The evaluated `OVERRIDES` chain; conditional operations, active or not; overrides that match nothing |
| `item` | The text of a pointer or of a named statement |
| `search` | Statements by name, regex or full text, each with its enclosing item |
| `class` | Which class file wins, its interface and tasks; who inherits it |
| `layers` | Layers, priorities and compatibility; shadowed recipes and classes; bbappend matches |
| `deps` | Build, runtime and `PACKAGECONFIG` dependencies; reverse dependencies; providers |
| `lint` | Convention and syntax findings for a recipe, file, layer or the workspace |
| `scaffold` | A new recipe, bbappend, layer or class, as a dry run or written |
| `bitbake` | Runs bitbake, devtool or bitbake-layers as a background job |
| `log` | A digest of a failed task's log |
| `slice` | Part of a stored output, by lines, regex, variable or failures |
| `status` | Workspace roots, release, index, build environment, running job |

Every argument and output format is in [docs/mcp-tools.md](docs/mcp-tools.md); the lint
rules are in [docs/lint-rules.md](docs/lint-rules.md). The server also offers the resources
`sous-chef://help` and `sous-chef://rules`.

## Commands

| Command | What it does |
|---|---|
| `sous-chef serve` | Serves MCP over stdio |
| `sous-chef index` | Refreshes the index and prints what changed |
| `sous-chef lint [TARGET] [--rule ID]... [--severity S] [--fail-on S] [--format text\|json] [--all]` | Lints a recipe, pointer, layer or path; default the writable layers and the build configuration. Exit status 0 clean, 1 a finding at or above `--fail-on` (default `error`), 2 lint could not run |
| `sous-chef hook` | Claude Code hook: reads one event on stdin, writes nothing or one response object; always exits 0 |

Global flags: `--build-dir DIR`, `--config FILE`, `--layer DIR` (repeatable).

## Hooks

All three are `PostToolUse` and fail open: any error, a missing index or more than 150 ms
means no output and the tool result unchanged.

- **Grep, Read**: hits in Yocto files get their enclosing item (`[do_install:append
  41-58]`) and, for a recipe, the bbappends and includes that also contribute; a Read that
  stops inside a function says where it ends. The output itself is not changed.
- **grep through Bash**: on Linux and macOS Claude Code has no Grep tool by default and
  searches with `grep` or `rg` through Bash. A single `grep -n` / `grep -rn` / `rg -n`
  command (no pipeline, line numbers asked for explicitly: `rg` prints none off a
  terminal) whose output is `<path>:<line>:<text>` gets the same annotations as context,
  for the first 200 hits (the context says when it stopped there). The output is not
  changed; any other command is left alone.
- **Bash**: the output of a `bitbake`, `devtool` or `bitbake-layers` command over 3 KiB is
  stored under a handle and replaced by a digest and a `slice` hint.
- **Edit, Write**: the edited file is reindexed and linted, and up to five findings at
  warning level or above are added; an edit in a layer that is not writable gets a line
  suggesting a bbappend, with the `scaffold kind=where` call to run.

Each can be switched off in `sous-chef.toml` (all default `true`); a switched-off hook
exits 0 with no output:

```toml
[hooks]
grep_read = false  # the Grep / Read annotation and the grep-through-Bash annotation
bash = false       # the BitBake output digest
edit = false       # the reindex and lint after an edit
```

## Limits

- **Static.** The index, `recipe`, `var`, `overrides`, `deps`, `lint` and `scaffold` read the
  layers themselves and need no build environment. Python is never evaluated statically:
  `${@...}` expressions, anonymous python and `d.setVar` in classes. Values that depend on
  them are marked (`depends on python, not evaluated`), and anonymous python that changes a
  variable is named in a warning.
- **Needs BitBake.** `bitbake`, `log`, and `var` with `authoritative: true` (which runs
  `bitbake -e`; documented as returning "bitbake is not available" until it is
  implemented) need an initialised build environment (`init_script`, `[bitbake]
  enabled`).
- **Not modelled.** `deps` does not resolve `BBCLASSEXTEND` variants such as `-native`;
  `PACKAGECONFIG` dependencies are listed from the flags, since OE-core adds them with
  anonymous python. Reverse dependencies see unconditional statements as written.
- **Writes** go only to layers listed in `writable_layers`; upstream layers are never
  edited. No arbitrary shell execution through MCP; no network.
- **Releases.** Wrynose (6.0 LTS) is the primary target: defaults, fixtures and scaffold
  output follow it. Scarthgap (5.0 LTS) is fully supported. Kirkstone (4.0 LTS) is the
  floor, best effort: it parses and release-dependent behaviour is switched correctly, but
  it has no fixture workspace. Older releases are out of scope. A layer is linted against
  every supported release in its `LAYERSERIES_COMPAT` and the workspace's own release.

The decisions and non-goals are in [docs/overview.md](docs/overview.md).

## Troubleshooting

- **Where things are.** The index is `$XDG_CACHE_HOME/sous-chef/<workspace key>/index.sqlite`
  (`~/.cache/sous-chef/...` without `XDG_CACHE_HOME`); the `status` tool prints its path
  and age. The hook's debug log, one line per handled call with the time taken and the
  outcome, is `hook.log` in the same directory (or `~/.cache/sous-chef/hook.log` before a
  workspace is known).
- **Rebuilding the index.** Every tool call refreshes it (files whose mtime or size
  changed are rehashed and reindexed); `sous-chef index` does the same from the shell. The
  index is derived and can be deleted: remove the workspace's cache directory and run
  `sous-chef index` to build it from scratch.
- **No annotations from the hooks.** The hooks never build the index: run `sous-chef index`
  (or use any tool) first. A file edited since the last refresh gets no Grep / Read
  annotation until the next one. Check `hook.log`, and run `claude --debug=hooks`.
  On Linux and macOS Claude Code leaves the Grep tool out by default and searches with
  `grep` through Bash: only a plain `grep -n` / `rg -n` command is annotated (see
  [Hooks](#hooks)); a pipeline, `rg` without `-n` or a one-file `grep -n` (no file name in
  its output) is not. Check `[hooks]` in `sous-chef.toml`.
- **Server not connected.** `sous-chef` must be on the `PATH` Claude Code sees
  (`claude mcp list` shows `Failed to connect` otherwise). A server that starts but cannot
  load the configuration still runs, and `status` reports the error.
- **Wrong workspace.** `status` prints `work_dir`, `build_dir` and the layers found. The
  plugin starts the server in the project directory (`$CLAUDE_PROJECT_DIR`); start Claude
  Code there, or put `build_dir` in `sous-chef.toml`.

## Documentation

[docs/README.md](docs/README.md) is the index: overview, architecture and contracts, the
BitBake semantics reproduced, the tools, the lint rules and the milestones.

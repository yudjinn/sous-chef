# sous-chef

`sous-chef` is a stdio MCP server, written in Rust, that helps an AI agent work on Yocto /
OpenEmbedded layers. It indexes every layer of a workspace into SQLite and answers with
**pointers** (`layer:path:A-B` plus a one-line snippet) instead of file contents. It replays
a recipe's files in BitBake's order, with its includes, classes, bbappends and the override
chain, so "where does `SRC_URI` get its value for this recipe on this machine" is one call.
It also lints layers against Yocto conventions, scaffolds recipes, bbappends, layers,
classes and `.inc` splits, advises where a change belongs, and runs BitBake when a build
environment exists (an OE init script, a bitbake-setup build or a kas project), keeping
large output on disk under a handle. Claude Code hooks make the index useful even when the
agent reaches for Grep, Read or Bash.

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
it, sous-chef walks up from the working directory to the first `conf/bblayers.conf`, then
tries a kas project's build directory, and failing that treats directories with a
`conf/layer.conf` as the layers. Paths are relative to the file; an unknown key is an
error. An example:

```toml
build_dir = "build"                   # where conf/bblayers.conf lives
writable_layers = ["meta-mycompany"]  # scaffold writes only here; lint checks these by default
release = "scarthgap"                 # optional; detected from the core layer otherwise
kas = ["kas/base.yml", "kas/board.yml"]  # optional; a kas project's files, in kas's order

[lint.severity]
"override/remove-in-recipe" = "info"

[hooks]
bash = false                          # see Hooks

[handles]                             # stored tool output (handles), cleaned at server start
max_total_mb = 500                    # keep at most this much (default 500)
max_age_days = 14                     # drop handles older than this (default 14)
```

Other keys: `layers` (extra layer roots, for a workspace without a build directory),
`init_script` (sourced before any bitbake command), `[bitbake] enabled` and
`allow_destructive`, `[lint] disable`, `[output] budget_tokens`, `[hooks] grep_read` and
`edit`. A `[handles]` value of 0 or out of range (`max_total_mb` 1 to 1000000,
`max_age_days` 1 to 3650) is a configuration error; `status` shows the store's size and
limits. See "Configuration" in [docs/architecture.md](docs/architecture.md).

**kas projects.** The kas files are found without `kas` when exactly one sits next to the
kas build directory, and with `writable_layers` empty the project's own layers are
writable. sous-chef reads the kas files as kas merges them (includes, lock files) and runs
no kas command to do so. `KAS_MACHINE`, `KAS_DISTRO`, `KAS_TARGET` and `KAS_TASK` in the
environment sous-chef was started in override the files as in kas (`status` shows each).
Before the first `kas checkout` (no build directory) the kas machine and distro stand in
for the `local.conf` kas would write. BitBake commands run through `kas shell
--keep-config-unchanged`, except when a kas file includes a file from another repository
(`{repo, file}`): kas fetches and checks out such repositories on every `kas shell`, even
with that option, so sous-chef then uses another way into the build environment, or none.

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
| `recipe` | A recipe's files, versions, bbappends, classes and key values; its file tree; its functions; its sources and patches, tasks in order, packages |
| `var` | The history and final value of a variable for a recipe, machine and distro; with `authoritative`, BitBake's own (`bitbake -e`) |
| `overrides` | The evaluated `OVERRIDES` chain; conditional operations, active or not; overrides that match nothing |
| `item` | The text of a pointer or of a named statement |
| `search` | Statements by name, regex or full text, each with its enclosing item |
| `class` | Which class file wins, its interface and tasks; who inherits it |
| `layers` | Layers, priorities and compatibility; shadowed recipes and classes; bbappend matches; compatibility, dependency and order problems |
| `deps` | Build, runtime and `PACKAGECONFIG` dependencies; reverse dependencies; providers |
| `lint` | Convention and syntax findings for a recipe, file, layer or the workspace, with MCP progress per file if the client asks for it |
| `scaffold` | A new recipe, bbappend, layer or class, or an `.inc` split, as a dry run or written; or where a change belongs |
| `bitbake` | Runs bitbake, devtool or bitbake-layers as a background job, in the build environment (init script, bitbake-setup or kas); poll with `op=status`, or pass `wait` to answer when it ends, with MCP progress notifications meanwhile if the client asks for them |
| `log` | A digest of a failed task's log |
| `slice` | Part of a stored output, by lines, regex, variable or failures |
| `status` | Workspace roots, release, index, build environment, kas project, running job |
| `audit` | One findings report over every layer of the repository (inventory with git facts and kas pins, releases, layer structure, lint, parse problems, what was not checked), stored under a handle; returns a digest |
| `diff` | Statement-level diff of a layer's recipes between two git revisions (variables, tasks, inherits, `SRC_URI` entries, version renames, changed classes) and which of your bbappends no longer apply, apply to another version or carry patches on changed source; read-only, no checkout |

Every argument and output format is in [docs/mcp-tools.md](docs/mcp-tools.md); the lint
rules are in [docs/lint-rules.md](docs/lint-rules.md). The server also offers the resources
`sous-chef://help` and `sous-chef://rules`.

## Commands

| Command | What it does |
|---|---|
| `sous-chef serve` | Serves MCP over stdio |
| `sous-chef index` | Refreshes the index and prints what changed |
| `sous-chef lint [TARGET] [--rule ID]... [--severity S] [--fail-on S] [--format text\|json] [--all]` | Lints a recipe, pointer, layer or path; default the writable layers and the build configuration. Exit status 0 clean, 1 a finding at or above `--fail-on` (default `error`), 2 lint could not run |
| `sous-chef audit [ROOT] [--format md\|json] [--output FILE]` | Writes one findings report for a whole build repository: every layer under ROOT (default: the working directory), enabled or not, with git facts, releases, layer structure, lint findings of every layer and parse problems. Read-only; takes about as long as `lint --all` (15 to 30 s on a large workspace), progress on stderr. Exit status 0 when the report was written, 2 when it could not be |
| `sous-chef diff LAYER FROM [TO] [--recipe PN] [--all] [--format text\|json] [--output FILE]` | The `diff` tool's whole table (or the whole diff as JSON) between two revisions of the layer's repository; `TO` defaults to the working tree. Read-only (no checkout, no fetch); about a second for every recipe of OE-core. Exit status 0 when the diff was produced, 2 when it could not be |
| `sous-chef hook` | Claude Code hook: reads one event on stdin, writes nothing or one response object; always exits 0 |

Global flags: `--build-dir DIR`, `--config FILE`, `--layer DIR` (repeatable).

## Hooks

All are `PostToolUse` and fail open: any error, a missing index or more than 150 ms
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
- **Bash**: the output of a `bitbake`, `devtool` or `bitbake-layers` command over 3 KiB
  (also run through `kas build` or `kas shell -c`) is stored under a handle and replaced by
  a digest and a `slice` hint.
- **Edit, Write**: the edited file is reindexed and linted, and up to five findings at
  warning level or above are added; an edit in a layer that is not writable gets a line
  suggesting a bbappend, with the `scaffold kind=where name=<recipe>` call to run when the
  file belongs to one recipe.

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
  variable is named in a warning. Measured against `bitbake -e` on a real Wrynose workspace
  (25 recipes, two of them `BBCLASSEXTEND` variants, 1090 values,
  [docs/differential.md](docs/differential.md)): 701 (64%) were exactly BitBake's, 1 more
  after normalising whitespace; the other 388 differed and said so (357 flagged as depending
  on python or unresolved references) or were a documented gap (29 path spellings, 2
  `FILE_LAYERNAME` of a variant, which `bitbake -e` shows differently from a build); no
  value differed without a flag or a documented cause.
- **Needs BitBake.** `bitbake`, `log`, and `var` with `authoritative: true` (which runs
  `bitbake -e` as a background job and caches the dump until the metadata changes) need a
  build environment: an OE `oe-init-build-env`, a bitbake-setup `init-build-env`, an
  environment the server was started in, or a kas project with `kas` on `PATH` and an
  existing build directory (not when a kas file includes another repository's file, see
  "kas projects") (`status` shows which).
- **Partly modelled.** `BBCLASSEXTEND` variants such as
  `gmp-native` are evaluated as BitBake makes them, but what OE-core's class-extension
  handlers and anonymous python filter (`DEPENDS`, `PACKAGES`, ...) is flagged, not
  computed, except the `native` variant's `PROVIDES` (computed on a static value, so
  `virtual/x-native` finds its provider); a static `BBMASK`
  marks the files it masks (with the default machine and distro; an evaluation for another
  machine or distro says which files its own mask would treat differently), and
  `SKIP_RECIPE`, `COMPATIBLE_MACHINE`, `COMPATIBLE_HOST`, `INCOMPATIBLE_LICENSE`,
  `features_check` and a `PREFERRED_PROVIDER_virtual/...` naming another recipe give a
  "BitBake would skip this recipe" note when the values involved are static, or "may skip"
  naming the unknown ones; nothing is hidden. `PACKAGECONFIG` dependencies are listed from
  the flags, since OE-core adds them with anonymous python. Reverse dependencies see
  unconditional statements as written. Tasks added by anonymous python are not seen. The
  `.inc` split evaluates every version before and after (the planned files held in memory)
  and refuses any change.
- **Writes** go only to layers listed in `writable_layers` (and, in a kas project, the kas
  file a new layer is added to); upstream layers are never edited. No arbitrary shell
  execution through MCP; no network.
- **Releases.** Wrynose (6.0 LTS) is the primary target: defaults, fixtures and scaffold
  output follow it. Scarthgap (5.0 LTS) is fully supported. Kirkstone (4.0 LTS) is the
  floor, best effort: it parses and release-dependent behaviour is switched correctly, but
  it has no fixture workspace. Older releases are out of scope. A layer is linted against
  every supported release in its `LAYERSERIES_COMPAT` and the workspace's own release.
- **Speed.** On a 1784-file workspace (OE-core, meta-poky, meta-yocto-bsp, meta-arm and two
  product layers) the first index build takes about 1.3 s and the refresh every tool call
  starts with about 19 ms ([docs/performance.md](docs/performance.md)). meta-openembedded
  scale was not measured.

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
  `sous-chef index` to build it from scratch. Set `XDG_CACHE_HOME` to keep an experiment's
  index out of `~/.cache`.
- **A recipe or file sous-chef calls "not collected".** A `.bb` / `.bbappend` that no
  layer's `BBFILES` glob matches, that only an inactive `BBFILES_DYNAMIC` entry matches, or
  that a static `BBMASK` masks is indexed but is no recipe; a `.conf` / `.inc` a recipe
  installs (a data file) is not searched or linted. `item` says which applies.
- **`authoritative: true` says it cannot run.** BitBake allows one client per build
  directory: while a build, task, clean or devtool job runs, authoritative `var` answers
  only from a cached dump (behind a `bitbake -e` or `bitbake-layers` job it waits up to
  30 s instead). Wait for the job (`bitbake op=status`) or use static mode.
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

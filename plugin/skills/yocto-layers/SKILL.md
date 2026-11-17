---
name: yocto-layers
description: Working on Yocto / OpenEmbedded / BitBake layers (recipes .bb, .bbappend, .inc, .bbclass, layer and build .conf files, kas projects) with the sous-chef MCP tools. Use it to look up recipes, variable values, overrides, dependencies and classes, to lint, scaffold, and run or debug bitbake, instead of grepping and reading files.
---

# Yocto layers with sous-chef

The sous-chef MCP server indexes every layer of the workspace. Ask it before reaching for
grep, find or Read: one call answers what would take many file reads, and it applies
BitBake's parse order, layer priority and override chain, which a grep does not.

## Which tool

| Question | Tool |
|---|---|
| What is this recipe: file, versions, bbappends, classes, key values | `recipe` (`show`: `summary`, `tree` of files in parse order, `outline` of functions and tasks, `sources` with patch files and their `Upstream-Status`, `tasks` in order, `packages`) |
| Final value of a variable and every operation on it, in order | `var` (`recipe`, `machine`, `distro`; `VAR[flag]`; `flags: true`) |
| Active overrides, conditional operations, misspelt overrides | `overrides` (`show`: `chain`, `conditional`, `dead`) |
| The text of a function or assignment | `item` (`loc`, or `recipe` + `name`, or `file` + `name`) |
| Where a name or text occurs | `search` (`mode`: `name`, `regex`, `fts`; filter by `kind`, `layer`, `recipe`) |
| A class: which file wins, its interface, who inherits it | `class` (`show`: `summary`, `users`, `outline`) |
| Layers, shadowed recipes and classes, bbappend matches, layer compatibility | `layers` (`show`: `list`, `shadowed`, `appends`, `compat`) |
| Build / runtime / PACKAGECONFIG deps, reverse deps, providers | `deps` (`kind`) |
| Convention and syntax problems | `lint` (`target`: recipe, layer, pointer or path) |
| New recipe, bbappend, layer, class; an `.inc` for shared versions; where a change belongs | `scaffold` (`kind`: `recipe`, `bbappend`, `layer`, `class`, `inc_split`, `where`) |
| Run bitbake, devtool, bitbake-layers | `bitbake` (`op`), then `op: status` to poll |
| Why a task failed | `log` |
| Read stored output | `slice` |
| Workspace roots, release, index, build environment | `status` |

## Pointers and handles

- Results are pointers `layer:path:A-B` (lines 1-based, inclusive, path relative to the
  layer root) with a one-line snippet. Expand only the ones you need with `item`; its text
  is the exact file content, usable as an Edit `old_string`.
- Large output (bitbake consoles, env dumps, long values) is stored under a handle
  `h:<10 hex>` and you get a digest. Read the rest with `slice` (`lines`, `grep`, `var`,
  `failures`, `page`).
- `+N more` at the end of a table means it was cut by `limit` or `budget_tokens`.

## Static values

`var`, `recipe`, `overrides` and `deps` replay the files statically. Python is never run:

- `${@...}` expressions stay unevaluated; the result says `depends on python, not
  evaluated` or `1 value depends on python ... use authoritative=true`.
- A warning that anonymous python changes a variable means the value shown is the one
  before that code runs; an opaque anonymous function may change anything.
- In those cases `var` with `authoritative: true` asks BitBake (`bitbake -e`, as a
  background job; a few seconds with a warm BitBake cache). It needs a build environment
  (`status` shows it). The dump is cached until a `.conf`, class, `.inc` or the recipe
  changes; while a build runs it answers from that cache or says it cannot run now.
- A note `no MACHINE is set` means machine-specific values were not applied: pass
  `machine`.

## Writing

- `scaffold` is a dry run that returns the planned files unless `write: true`. It writes
  only in layers listed in `writable_layers`, and lints what it wrote.
- Not sure where a change belongs? `scaffold kind=where name=<recipe>` with a `variable`,
  `task` or `intent` says which file (recipe, bbappend, machine or distro conf, image) and
  the call that makes the change; it writes nothing.
- To change a recipe of an upstream layer, add a bbappend in a writable layer
  (`scaffold kind=bbappend`), not an edit of the upstream file.
- `scaffold kind=inc_split` moves what several versions of your recipe share into
  `<pn>.inc`; it refuses when the move could change what BitBake parses.
- `bitbake` starts a background job and returns `job:<n>` at once; one job runs at a time.
  `cleansstate`, `cleanall`, `devtool reset` and `devtool finish` need `confirm: true` and
  `allow_destructive` in `sous-chef.toml`.

## Releases

The workspace release (shown by `status` and `layers`) is `release` in `sous-chef.toml`,
else detected from the core layer's `LAYERSERIES_CORENAMES`. A layer is linted against
every release it claims in `LAYERSERIES_COMPAT` plus the workspace's, and a finding names
the release it applies to. Scaffold output follows the same releases.

## Hooks

With the plugin's hooks on: Grep and Read results on Yocto files get the enclosing item,
and for a recipe the bbappends and includes that also contribute; large `bitbake`,
`devtool` or `bitbake-layers` output in Bash (also through `kas build` or `kas shell -c`) is
replaced by a digest and a handle; after an Edit or Write of a Yocto file its lint findings
are added. A plain `grep -rn` or `rg -n`
run through Bash (no pipe; line numbers asked for) gets the same annotation as Grep, for
its first 200 hits; anything else run through Bash gets none: use `search`.

## kas projects

A project set up with kas (<https://kas.readthedocs.io>) keeps its configuration in kas
YAML files, and kas writes `conf/local.conf` and `conf/bblayers.conf` in its build
directory (`build/` by default) from them, again on every checkout, shell and build. Make
a configuration change in the kas file (`local_conf_header`, `machine`, `distro`, the repos
and layers), not in the generated `local.conf` or `bblayers.conf`; lint reports edits there
(`org/generated-conf-edit`). sous-chef finds the project (`kas` in `sous-chef.toml`, or the
only kas file next to a kas build directory) and `status` shows its files, machine, distro
and layers. `scaffold kind=layer` adds a new layer to the kas file, not to `bblayers.conf`.
With `kas` on `PATH` the `bitbake` tool runs commands through `kas shell
--keep-config-unchanged`; by hand: `kas shell <file.yml>[:<more.yml>] -c '<bitbake
command>'`.

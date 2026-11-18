# sous-chef

sous-chef is an MCP server and Claude Code plugin that gives an AI agent an index and a
static evaluator of the Yocto / OpenEmbedded layers of a workspace. It answers with
**pointers** into the layers (`layer:path:A-B` plus a one-line snippet) instead of whole
files, and it replays a recipe's files the way BitBake does (includes, inherits, bbappends
in layer order, then the override chain), so "where does `SRC_URI` get its value for this
recipe on this machine" is one call with a correct answer.

It is a single Rust binary that speaks MCP over stdio, with optional hooks for Claude Code.
It needs no build environment for lookups; it uses BitBake only when asked to run it.

## What it does

- **Find and read recipe items.** A recipe's files, versions (and which one BitBake picks,
  and why), bbappends in application order, classes and key values; its file tree,
  functions, sources and patches, tasks and packages, and what its metadata declares about
  CVEs (`recipe`). The exact text of a
  function or assignment, usable as an Edit string (`item`). Statements by name, literal text, regex or
  full text, each with its enclosing item (`search`). Which class file wins and who
  inherits it (`class`).
- **The final value of a variable and where every part comes from.** Every operation in
  parse order, which apply for a machine and distro, and the result (`var`; BitBake's own
  with `authoritative: true`). The `OVERRIDES` chain and overrides that match nothing, such
  as a misspelt machine (`overrides`).
- **Dependencies and providers.** Build, runtime and `PACKAGECONFIG` dependencies, reverse
  dependencies, which recipe provides `virtual/kernel` and why (`deps`); layers,
  priorities, shadowed recipes, which bbappend applies to what (`layers`).
- **Lint against Yocto conventions**, each layer against every release its
  `LAYERSERIES_COMPAT` claims (`lint`, `sous-chef lint`), with fixes as a diff or written
  (`--fix`, `--write`), a check against a release the layer does not claim yet
  (`--as-release`) or a migration report to it (`--migrate-to`), SARIF and GitHub
  annotations for CI (`--format sarif|github`) and a baseline of accepted findings
  (`--baseline`). The answer says what it checked: conventions and release compatibility,
  not whether the build works.
- **Multiconfig**: `var`, `overrides`, `deps` and `recipe` evaluate in a `BBMULTICONFIG`
  configuration (`mc`, or `mc:<name>:<recipe>`).
- **Scaffold** recipes, bbappends, layers, classes and `.inc` splits into a writable layer,
  and say where a change belongs (`scaffold`).
- **Run BitBake and digest its logs.** `bitbake`, `devtool` and `bitbake-layers` as
  background jobs (`bitbake`); a failed task's log digested (`log`); large output kept
  under a handle and read in parts (`slice`).
- **Why did this task re-run?** A digest of `bitbake-diffsigs`: the variables that
  changed, each with the assignment that sets it now, and the dependency path (`sigs`).
- **Which package ships this file?** From an existing build's package data: the
  package, its recipe and the `FILES` assignment; a package's files, version and runtime
  dependencies (`pkgdata`).
- **Audit a build repository**: one findings report over every layer (`audit`,
  `sous-chef audit`).
- **Diff a layer between two revisions**, and see which of your bbappends no longer apply
  or carry patches on changed source (`diff`, `sous-chef diff`).
- **Hooks that make Grep, Read, Bash and edits Yocto-aware**: hits get their enclosing
  item, large BitBake output run through Bash becomes a digest, an edited file is linted.
- **kas projects**: the kas files are read as kas merges them, and BitBake runs through
  `kas shell`.

## Why use it

Yocto metadata is spread over many layers: a recipe's behaviour comes from the `.bb`, its
`.inc` files, the classes it inherits, bbappends in other layers and several configuration
files, and the value of a variable depends on parse order, layer priority and the override
chain. An agent that reads files to answer one question spends its context on them and can
still get the override chain wrong, because a grep shows assignments, not which one wins.
`bitbake -e` gives the answer, but for one recipe it is megabytes of output.

### Token savings

sous-chef answers with pointers and short tables instead of file contents. The sizes
below were measured on a real workspace (bootlin/yocto-kiss on Wrynose, OE-core and
meta-arm; see [docs/live-environment.md](docs/live-environment.md)), each sous-chef answer
against what an agent would otherwise read to answer the same question. Tokens are
estimated at 4 bytes each.

| Question | Without sous-chef | With sous-chef | Reduction |
|---|---|---|---|
| Where does `curl`'s `SRC_URI` come from? | The recipe and the classes and includes it pulls in: 127 KB, ~32k tokens; or `bitbake -e curl`: 1.9 MB, ~470k tokens | `var SRC_URI recipe=curl`: 3.7 KB, ~900 tokens | ~35× against the files, ~500× against `bitbake -e` |
| What is `curl` (versions, appends, key values)? | `curl_8.19.0.bb`: 9.4 KB, ~2.4k tokens | `recipe curl`: 1.5 KB, ~370 tokens | ~6× |
| What does `curl` pull in? | The same 127 KB of files | `recipe curl show=tree`: 1.4 KB | ~90× |
| Why did a task fail? | A real `log.do_fetch`: 48 KB, ~12k tokens | the `log` digest: 523 bytes, ~130 tokens | ~90× |
| Large BitBake output run through Bash | Console output of a small failed fetch: 5.6 KB | the Bash hook's digest: 716 bytes, full text kept under a handle | ~8× (more for real builds) |
| Where is `PACKAGECONFIG[gnutls]` set? | `grep -rn`: about the same size | `search`: 1.2 KB for 8 hits | none |

- The savings are largest where BitBake combines files: final values and where they come
  from, recipe trees, task composition, build failures.
- A search answer is about the size of grep's; the gain there is that each hit names the
  item it is in, which saves follow-up reads.
- The hooks add a few hundred bytes of context to each annotated Grep or Read.
- A table is cut to `budget_tokens` (600 by default, 1200 for `search`, at 4 bytes a token)
  with a marker saying how many rows were left out and the budget that returns them all;
  `item` and `slice` text is capped at 8 KB; long
  values, logs, diffs and reports go under a handle read with `slice`.
- These are per-question measurements.

**Whole tasks.** A pilot ran 14 read-only questions about the same workspace as real Claude
Code sessions (Claude Code 2.1.327, Haiku 5.5, one session per task and condition), with
and without the plugin ([docs/benchmark.md](docs/benchmark.md)). Without sous-chef the
sessions used 1.28 times the tokens (1,919,410 against 1,505,217), 199 against 128 turns,
and scored 0.60 against 0.83 on the reference facts. Per task the ratio ranged from 0.23 to
2.52: sous-chef used fewer tokens on 8 tasks and more on 6, among them a simple search
(4.3 times more). The plugin added 9,356 tokens to every session's context in the pilot
(its sessions loaded every tool definition upfront). Since then the `tools/list` payload
was cut from 31.5 KB to 14.1 KB and the six core tools are marked for upfront loading under
Claude Code's tool search (docs/mcp-tools.md, "Context cost"); re-measured the same way
("Say ok.", Claude Code 2.1.327, Haiku 5.5, one session per condition), the plugin now adds
3,694 tokens with tool search on, Claude Code's default (six tools upfront, ten deferred),
and 5,693 with it off as in the pilot (docs/benchmark.md, "Plugin overhead after the
definition cuts"). One run per cell
is a pilot: single tasks moved by up to a factor of 2.3 between two runs. Part of the score
gap comes from the workspace itself: its `.gitignore` lists the upstream layers, and Claude
Code's Grep and Glob honour it, so three baseline sessions did not find OE-core at all.

### Accuracy

Static answers were compared with `bitbake -e` on the same workspace (yocto-kiss, Wrynose,
MACHINE `dogbonedark`, DISTRO `kiss`; 23 recipes and two `BBCLASSEXTEND` variants;
[docs/differential.md](docs/differential.md); re-measured for x7-cleanup on the same dumps):

| Values compared | Equal to BitBake | Differ, and flagged | Documented gap | Differ without a flag |
|---|---|---|---|---|
| 1090 | 950 (87%) | 109 | 31 | 0 |

(Before x1 evaluated the allowlisted python helpers: 702 equal, 357 flagged.)

A flagged value carries `depends on python, not evaluated` or names an unresolved
reference; for those, `var` with `authoritative: true` asks BitBake. The 31 gaps are 29
paths that BitBake spells through bitbake-setup's symlinked `layers/` directory and
sous-chef through the symlink's target (the same files), and 2 `FILE_LAYERNAME` values of a
variant, which `bitbake -e <variant>` shows differently from a build. The measurement covers one
machine, one distro and one release; functions and variable flags were not compared.

### Speed

On the same workspace (1784 files: OE-core, meta-poky, meta-yocto-bsp, meta-arm and two
product layers; [docs/performance.md](docs/performance.md), "After the later rounds"):
the first index build takes 1.2 s; the refresh every tool call starts with takes 19 ms
when nothing changed; `var SRC_URI recipe=curl` answers in 29 ms; the Grep, Read and Edit
hooks take 4.6, 3.5 and 9.2 ms (process wall time, against a 150 ms budget).
meta-openembedded scale was not measured.

### What it does not do

Python is not run. A `${@...}` expression made only of an allowlist of pure helpers
(`bb.utils.contains`, `oe.utils.conditional`, `d.getVar`, string methods, ...; the list
and each helper's source are in docs/bitbake-semantics.md, "Inline python evaluated
statically") is evaluated by sous-chef's own interpreter with python's semantics, and the
answer says it was; anything else in `${@...}`, every anonymous python function, python
tasks and anything that would read the host (files, the environment, the clock) is never
evaluated, and an answer that depends on it says so instead of guessing. sous-chef never builds anything by itself: BitBake runs only when the `bitbake`
tool (or authoritative `var`) is called, and the hooks never start it. It writes only to
the layers you mark writable, runs no arbitrary shell command through MCP, and makes no
network connection itself (a BitBake build it starts may fetch, as any build does).

## How to use it

### Install

**The binary.** Needs a recent stable Rust toolchain (the crate uses edition 2024). SQLite
is bundled.

```sh
cargo install --path .        # installs sous-chef into ~/.cargo/bin
sous-chef --version
```

The plugin and the MCP configurations below run `sous-chef` by name, so `~/.cargo/bin`
(or wherever the binary is) must be on the `PATH` Claude Code sees.

**The Claude Code plugin.** [`plugin/`](plugin/) bundles the MCP server, the hooks and a
skill for working on layers. It is also a one-plugin marketplace, so it installs from a
clone:

```sh
claude plugin marketplace add ./plugin
claude plugin install sous-chef@sous-chef
```

(Inside a session: `/plugin marketplace add ./plugin`, then `/plugin install
sous-chef@sous-chef`.) The plugin is read in place from `plugin/`, so a `git pull` updates
it. To try it for one session without installing: `claude --plugin-dir ./plugin`.

Start Claude Code in the project directory (the one with `sous-chef.toml`, or the build
directory's parent): the plugin starts the server there (`$CLAUDE_PROJECT_DIR`). `/mcp`
should show `plugin:sous-chef:sous-chef` connected; its tools are
`mcp__plugin_sous-chef_sous-chef__<tool>`. What the plugin registers, and what was checked
against which Claude Code version, is in [plugin/README.md](plugin/README.md).

**Without the plugin.** Any MCP client can run the server over stdio. The server finds the
workspace from its working directory, so give it the configuration explicitly when the
client starts it elsewhere:

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

### A minimal `sous-chef.toml`

Without one, sous-chef uses the nearest `conf/bblayers.conf` at or above the working
directory, then a kas project's build directory, and failing both treats the directories
with a `conf/layer.conf` below the working directory as the layers. The plugin starts the
server in the project directory, usually the build directory's parent, so put a
`sous-chef.toml` there that names the build directory and your own layers (`scaffold`
writes only to those, and `lint` checks them by default):

```toml
build_dir = "build"                   # where conf/bblayers.conf lives
writable_layers = ["meta-mycompany"]
```

All keys are under [Configuration](#configuration).

### Example prompts

You do not call the tools yourself: you ask the agent, and it picks them. Five common
cases, with the tools an agent typically reaches for and what comes back. Recipe, layer and
machine names are examples; use your own.

**1. Why does a variable have this value?**

> Why does `EXTRA_OECONF` for curl contain `--without-libpsl` on machine `dogbonedark`,
> and which file adds each part?

`var` (`name: "EXTRA_OECONF"`, `recipe: "curl"`, `machine: "dogbonedark"`) returns every
operation in parse order, across the recipe, its `.inc` files, classes and bbappends, with
a pointer each and whether it applies, and the final value; `overrides` shows the
`OVERRIDES` chain when the answer turns on a machine or distro override. A part that
depends on python sous-chef does not evaluate is flagged as such, not guessed.

**2. Change an upstream recipe from my own layer.**

> I want curl built with c-ares for our product. Where should that change go, and can you
> set it up in `meta-mycompany`?

`deps kind=packageconfig` (`recipe: "curl"`) shows that c-ares is the `ares` option of
curl's `PACKAGECONFIG` and what it adds; `scaffold kind=where name=curl` says where the
change belongs (a bbappend in your writable layer, with the reason, or an existing
bbappend to add to) and the call that makes it; `scaffold kind=bbappend` then plans the
file with `PACKAGECONFIG:append = " ares"`. It is a dry run that shows the
content until you agree to `write: true`, and it writes only into layers listed in
`writable_layers`. `recipe show=sources` and `item` show what the recipe has today.

**3. A build failed.**

> `bitbake curl` failed. What went wrong, and where is the code that failed?

With the plugin's Bash hook, the large BitBake output is replaced by a digest: the failing
recipe and task, the first error with context, and a pointer to the task's definition in
the layers. The full output is kept under a handle that `slice` reads in parts. Without
the hook, `log` (`recipe: "curl"`, `task: "compile"`) digests the task log the same way;
`sigs` answers the related question "why did this task re-run at all".

**4. What does this recipe pull in, and who provides it?**

> Which kernel recipe is used for `dogbonedark`, and why that one? And what does curl
> depend on at build time and at run time?

`deps kind=providers` (`recipe: "virtual/kernel"`) names the chosen provider, the
`PREFERRED_PROVIDER` that chose it with its pointer, and the others that were not chosen;
`deps kind=build` and `kind=runtime` list curl's dependencies, each with the assignment
that adds it; `deps kind=packageconfig` shows what each `PACKAGECONFIG` option would add.

**5. Is my layer in good shape, and what breaks on the next release?**

> Check `meta-mycompany` against Yocto conventions. We are moving from Scarthgap to Wrynose:
> what will break, and what can be fixed automatically?

`lint` reports findings with pointers, checked against every release the layer claims;
`lint` with `migrate_to: "wrynose"` lists what is new with that release, the features it
changed that the layer uses, and the `LAYERSERIES_COMPAT` line to write; `lint` with
`fix: true` shows the safe fixes as a diff before anything is written. For the whole
repository, `audit` writes one report over every layer.

Other questions that work well: "what does `do_install` of busybox consist of, including
its appends" (`recipe show=tasks`, `item`), "which package ships `/usr/bin/curl`"
(`pkgdata`, after a build), "what changed in OE-core between `yocto-5.0` and `yocto-6.0`
that affects our bbappends" (`diff`), "which CVEs does curl's metadata mark as patched"
(`recipe show=cve`).

### First steps

Ask the agent what you would ask a colleague who knows the layers: "what is `hello` and
where do its bbappends come from", "why is `EXTRA_OECONF` of `hello` what it is on
`boardx`", "what does `app`'s `do_install` look like", "lint my layer". The agent reaches
for `recipe`, `var`, `item` and `lint` first; `status` shows what sous-chef found (roots,
release, layers, build environment).

Output from the fixture workspace in `tests/fixtures/ws` (from the snapshots in
`tests/snapshots/`). `recipe` with `name: "hello"`:

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
note: key values are evaluated for MACHINE=qemux86 DISTRO=testdistro (static replay of the configuration, classes, recipe and bbappends; only allowlisted python helpers evaluated); use var for the full history
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

`item` with `loc: "meta-mine:recipes-app/app/app_0.1.bb:25-29"`: the exact lines, with the
hash of the file as read.

```
meta-mine:recipes-app/app/app_0.1.bb:25-29 sha256=8c263137e2cb
fakeroot do_install() {
	install -d ${D}${bindir}
	install -m 0755 app ${D}${bindir}/app
	chown root:root ${D}${bindir}/app
}
```

`sous-chef lint` in the same directory reports four findings in `meta-mine` and the build
configuration ([docs/lint-rules.md](docs/lint-rules.md), "Findings on the fixture
workspace").

### Commands

| Command | What it does |
|---|---|
| `sous-chef serve` | Serves MCP over stdio |
| `sous-chef index` | Refreshes the index and prints what changed |
| `sous-chef lint [TARGET] [--rule ID]... [--severity S] [--fail-on S] [--format text\|json\|sarif\|github] [--all] [--as-release R [--only-release]] [--migrate-to R] [--baseline FILE \| --write-baseline FILE]` | Lints a recipe, pointer, layer or path; default the writable layers and the build configuration (`--all`: every layer). `--as-release` also checks against a release the layers do not claim; `--migrate-to` prints the migration report; SARIF, annotations and the baseline are for CI ([docs/ci.md](docs/ci.md)). Exit status 0 clean, 1 a finding at or above `--fail-on` (default `error`; with `--baseline`, a new one), 2 lint could not run |
| `sous-chef audit [ROOT] [--format md\|json] [--output FILE] [--migrate-to R]` | Writes one findings report for a whole build repository: every layer under ROOT (default: the working directory), enabled or not, with git facts, releases, layer structure, lint findings of every layer and parse problems (`--migrate-to`: plus section 7, the migration report of the writable layers). Read-only; takes about as long as `lint --all` (15 s on the live workspace), progress on stderr. Exit status 0 when the report was written, 2 when it could not be |
| `sous-chef diff LAYER FROM [TO] [--recipe PN] [--all] [--format text\|json] [--output FILE]` | The `diff` tool's whole table (or the whole diff as JSON) between two revisions of the layer's repository; `TO` defaults to the working tree. Read-only (no checkout, no fetch); under a second for every recipe of OE-core. Exit status 0 when the diff was produced, 2 when it could not be |
| `sous-chef hook` | Claude Code hook: reads one event on stdin, writes nothing or one response object; always exits 0 |

Global flags: `--build-dir DIR`, `--config FILE`, `--layer DIR` (repeatable).

### Migrating a layer

"What breaks if this layer moves to release X?" for a release it does not claim yet:

```sh
sous-chef lint --migrate-to wrynose meta-mine      # or --format json; audit --migrate-to for every writable layer
```

The report lists the findings that hold for Wrynose and not for the releases the layer
claims now ("new with wrynose") apart from those that already hold, the features Wrynose
changed that the layer uses (each with the migration guide's instruction and every
pointer: `S = "${WORKDIR}"`, `${WORKDIR}/<file>`, class directories, `inherit_defer`,
removed classes and variables...), the layer's `LAYERSERIES_COMPAT` line and what to
write, and what it cannot tell (anything that needs a build). `--as-release wrynose` lints
as if the layer also claimed Wrynose. What each release removed, and what lint detects of
it, is the "Release removals" table in [docs/lint-rules.md](docs/lint-rules.md).

### In CI

`sous-chef lint --format sarif` writes SARIF 2.1.0 for GitHub code scanning, `--format
github` writes workflow annotations, and `--write-baseline FILE` / `--baseline FILE` keep
existing findings from failing new changes. Workflow examples (GitHub Actions, GitLab) and
the exit codes: [docs/ci.md](docs/ci.md).

### Fixing findings

 `sous-chef lint --fix [TARGET]` prints, per file, a unified diff of
the fixes it can make and a summary (fixes available, safe and suggested, by rule); nothing
is written until `--write`. Safe fixes preserve meaning or are exactly what BitBake requires
(legacy `_` override names to `:`, the missing space of an `:append`, whitespace after a
trailing `\`, `S = "${WORKDIR}"` to `S = "${UNPACKDIR}"` and the pure renames of removed
variables when every release the layer claims has the new form, ...); suggested ones change
meaning the way the rule recommends and are applied only with `--fix-suggested`. Writing
goes through the scaffold engine: writable layers only, a file changed since it was read is
refused, then the changed files are linted again, fixes that became possible are applied in
further rounds (up to five; the dry run shows the first), and any new finding is reported.
The `lint` tool takes `fix`, `fix_suggested` and `write`. Which rules have a fix: [docs/lint-rules.md](docs/lint-rules.md), "Fixes".

### kas projects

A kas project needs no extra setup when exactly one kas file sits next to the kas build
directory; otherwise list the files with `kas = [...]` in `sous-chef.toml`, in kas's order.
With `writable_layers` empty, the project's own layers are writable.

- sous-chef reads the kas files as kas merges them (includes, lock files) and runs no kas
  command to do so. `KAS_MACHINE`, `KAS_DISTRO`, `KAS_TARGET` and `KAS_TASK` in the
  environment sous-chef was started in override the files as in kas (`status` shows each).
- Before the first `kas checkout` (no build directory) the kas machine and distro stand in
  for the `local.conf` kas would write.
- Make configuration changes in the kas file, not in the `local.conf` or `bblayers.conf`
  kas generates (lint reports edits there: `org/generated-conf-edit`); `scaffold kind=layer`
  adds a new layer to the kas file.
- BitBake commands run through `kas shell --keep-config-unchanged`, except when a kas file
  includes a file from another repository (`{repo, file}`): kas fetches and checks out such
  repositories on every `kas shell`, even with that option, so sous-chef then uses another
  way into the build environment, or none (`status` says which).
- kas has no multiconfig setting of its own: a target `mc:<name>:<image>` makes kas write
  the name into `BBMULTICONFIG`. Before the first checkout sous-chef takes the
  multiconfigs from those targets (see [Multiconfig](#multiconfig)).

### Multiconfig

A build directory can hold several configurations (`BBMULTICONFIG = "tiny musl"` in
`local.conf`, one `conf/multiconfig/<name>.conf` each). `status` lists them with their files,
and reports a listed name without a file (BitBake stops on it). `var`, `overrides`, `deps`
and `recipe` take `mc=<name>`, or a recipe written `mc:<name>:<recipe>`: the configuration
is parsed again with the multiconfig file where OE-core's `bitbake.conf` reads it, so its
`MACHINE`, `TMPDIR` and `PREFERRED_PROVIDER_*` apply, and the answer says which configuration
it is for. `deps kind=mcdepends` lists the cross-configuration task dependencies. The
`bitbake` tool accepts `mc:<name>:<target>` targets. This was built from the BitBake,
OE-core and kas source and fixture tests, not checked against a running BitBake.

## Reference

### Configuration

`sous-chef.toml` is optional: `--config FILE`, else `sous-chef.toml` in the working
directory. Without one, sous-chef walks up from the working directory to the first
`conf/bblayers.conf`, then tries a kas project's build directory, and failing that treats
directories with a `conf/layer.conf` as the layers. Paths are relative to the file; an
unknown key is an error. Every key, with its default:

```toml
build_dir = "build"                   # where conf/bblayers.conf lives; default: discovered
layers = ["../meta-extra"]            # extra layer roots, for a workspace without a build directory
writable_layers = ["meta-mycompany"]  # scaffold writes only here; lint checks these by default
init_script = "poky/oe-init-build-env"  # sourced before any bitbake command; default: none, the environment is discovered
release = "scarthgap"                 # default: detected from the core layer
kas = ["kas/base.yml", "kas/board.yml"]  # a kas project's files, in kas's order; default: detected

[bitbake]
enabled = true                        # false: never run BitBake
allow_destructive = false             # allow cleansstate, cleanall, devtool reset, devtool finish

[lint]
disable = ["style/variable-order"]    # rule ids or groups; default: none

[lint.severity]                       # rule id or group to info, warning or error
"override/remove-in-recipe" = "info"

[output]
budget_tokens = 600                   # default output budget of every tool

[hooks]                               # all default true; see Hooks
grep_read = true
bash = true
edit = true

[handles]                             # stored tool output, cleaned at server start
max_total_mb = 500                    # keep at most this much (1 to 1000000)
max_age_days = 14                     # drop handles older than this (1 to 3650)
```

The flags `--build-dir` and `--layer` replace `build_dir` and `layers`. A `[handles]` value
out of range is a configuration error; a server whose configuration does not load still
starts, and every tool and `status` report the error. Details: "Configuration" in
[docs/architecture.md](docs/architecture.md).

### Tools

| Tool | What it answers |
|---|---|
| `recipe` | A recipe's files, versions, bbappends, classes and key values; its file tree; its functions; its sources and patches, tasks in order, packages; its CVE view (`CVE_PRODUCT`, `CVE_STATUS`, CVE patches, inconsistencies, a build's CVE report if there is one) |
| `var` | The history and final value of a variable for a recipe, machine and distro; in a multiconfig with `mc`; with `authoritative`, BitBake's own (`bitbake -e`) |
| `overrides` | The evaluated `OVERRIDES` chain; conditional operations, active or not; overrides that match nothing |
| `item` | The text of a pointer or of a named statement |
| `search` | Statements by name (also `NAME[flag]`), literal text, regex or full text, in one or several layers, each with its enclosing item |
| `class` | Which class file wins, its interface and tasks; who inherits it |
| `layers` | Layers, priorities and compatibility; shadowed recipes and classes; bbappend matches; compatibility, dependency and order problems |
| `deps` | Build, runtime and `PACKAGECONFIG` dependencies; reverse dependencies; providers; cross-configuration (`mcdepends`) dependencies |
| `lint` | Convention, syntax and release findings for a recipe, file, layer or the workspace, and what was checked; fixes (`fix`, `fix_suggested`, `write`), another release (`as_release`, `only_release`), a migration report (`migrate_to`); MCP progress per file if the client asks for it |
| `scaffold` | A new recipe, bbappend, layer or class, or an `.inc` split, as a dry run or written; or where a change belongs |
| `bitbake` | Runs bitbake, devtool or bitbake-layers as a background job, in the build environment (init script, bitbake-setup or kas); poll with `op=status`, or pass `wait` to answer when it ends, with MCP progress notifications meanwhile if the client asks for them |
| `log` | A digest of a failed task's log |
| `slice` | Part of a stored output, by lines, regex, variable or failures |
| `status` | Workspace roots, release, index, build environment, kas project, multiconfigs, running job, handle store |
| `audit` | One findings report over every layer of the repository (inventory with git facts and kas pins, releases, layer structure, lint, parse problems, what was not checked), stored under a handle; returns a digest |
| `diff` | Statement-level diff of a layer's recipes between two git revisions (variables, tasks, inherits, `SRC_URI` entries, version renames, changed classes) and which of your bbappends no longer apply, apply to another version or carry patches on changed source; read-only, no checkout |
| `sigs` | Why a task re-ran: a digest of `bitbake-diffsigs` (root-cause variables with the pointer of the assignment that sets them, the dependency path, taints), full output under a handle; builds nothing |
| `pkgdata` | From a build's package data: which package ships a file (with the recipe's `FILES` assignment), a package's files, its recipe, a recipe's packages, a package's version, size and runtime dependencies; builds nothing |

Every argument and output format is in [docs/mcp-tools.md](docs/mcp-tools.md); the lint
rules are in [docs/lint-rules.md](docs/lint-rules.md). The server also offers the resources
`sous-chef://help` and `sous-chef://rules`.

### Hooks

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

Each can be switched off in `sous-chef.toml`; a switched-off hook exits 0 with no output:

```toml
[hooks]
grep_read = false  # the Grep / Read annotation and the grep-through-Bash annotation
bash = false       # the BitBake output digest
edit = false       # the reindex and lint after an edit
```

### Limits

- **Static.** The index, `recipe`, `var`, `overrides`, `deps`, `lint` and `scaffold` read the
  layers themselves and need no build environment. Inline python of the allowlisted pure
  helpers is evaluated (and said so); other inline python is marked (`depends on python, not
  evaluated`), and anonymous python, which is never run, is named in a warning when it
  changes a variable (see [Accuracy](#accuracy)).
- **Needs BitBake.** `bitbake`, `log`, and `var` with `authoritative: true` (which runs
  `bitbake -e` as a background job and caches the dump until the metadata changes) need a
  build environment: an OE `oe-init-build-env`, a bitbake-setup `init-build-env`, an
  environment the server was started in, or a kas project with `kas` on `PATH` and an
  existing build directory (not when a kas file includes another repository's file, see
  [kas projects](#kas-projects)); `status` shows which.
- **Partly modelled.** `BBCLASSEXTEND` variants such as `gmp-native` are evaluated as
  BitBake makes them, but what OE-core's class-extension handlers and anonymous python
  filter (`DEPENDS`, `PACKAGES`, ...) is flagged, not computed, except the variants'
  `PROVIDES`. A static `BBMASK` is applied; `SKIP_RECIPE`, `COMPATIBLE_MACHINE`,
  `COMPATIBLE_HOST`, `INCOMPATIBLE_LICENSE`, `features_check` and a
  `PREFERRED_PROVIDER_virtual/...` naming another recipe give a "BitBake would skip this
  recipe" note when the values involved are static, or "may skip" naming the unknown ones;
  nothing is hidden. `PACKAGECONFIG` dependencies are listed from the flags. Tasks added by
  anonymous python are not seen. Lint, the index's `BBMASK` marks and the audit are for the
  default configuration only (not per [multiconfig](#multiconfig)), and the authoritative
  `var` does not take one. The details are in [docs/mcp-tools.md](docs/mcp-tools.md)
  (the common section) and [docs/bitbake-semantics.md](docs/bitbake-semantics.md) ("What
  the static evaluator cannot do").
- **Writes** go only to layers listed in `writable_layers` (and, in a kas project, the kas
  file a new layer is added to); upstream layers are never edited. The `.inc` split
  evaluates every version before and after and refuses any change.
- **Releases.** Wrynose (6.0 LTS) is the primary target: defaults, fixtures and scaffold
  output follow it. Scarthgap (5.0 LTS) is fully supported. Kirkstone (4.0 LTS) is the
  floor, best effort: it parses and release-dependent behaviour is switched correctly, but
  it has no fixture workspace. Older releases are out of scope. A layer is linted against
  every supported release in its `LAYERSERIES_COMPAT` and the workspace's own release.

The decisions and non-goals are in [docs/overview.md](docs/overview.md).

### Troubleshooting

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
  Through Bash only a plain `grep -n` / `rg -n` command is annotated (see [Hooks](#hooks));
  a pipeline, `rg` without `-n` or a one-file `grep -n` (no file name in its output) is
  not. Check `[hooks]` in `sous-chef.toml`.
- **Server not connected.** `sous-chef` must be on the `PATH` Claude Code sees
  (`claude mcp list` shows `Failed to connect` otherwise). A server that starts but cannot
  load the configuration still runs, and `status` reports the error.
- **Wrong workspace.** `status` prints `work_dir`, `build_dir` and the layers found. The
  plugin starts the server in the project directory (`$CLAUDE_PROJECT_DIR`); start Claude
  Code there, or put `build_dir` in `sous-chef.toml`.

### Measuring token use

To measure your own workspace, `scripts/measure-tokens.py` calls the tools over stdio and
prints each answer's size (it runs no BitBake and writes only an index cache; the
[Token savings](#token-savings) table was produced this way):

```sh
scripts/measure-tokens.py --config sous-chef.toml --sizes-only \
    var '{"name":"SRC_URI","recipe":"curl"}' \
    recipe '{"name":"curl","show":"tree"}'
```

Compare each size with `wc -c` of the files the same question needs (the recipe and the
files `recipe show=tree` lists, or a task log under `${T}`).

### Documentation

[docs/README.md](docs/README.md) is the index. In short:

| Document | What it holds |
|---|---|
| [docs/overview.md](docs/overview.md) | The problem, the decisions, non-goals, glossary |
| [docs/mcp-tools.md](docs/mcp-tools.md) | Every tool: arguments, output, examples |
| [docs/lint-rules.md](docs/lint-rules.md) | Every lint rule, and what lint reports on the fixture and live workspaces |
| [docs/ci.md](docs/ci.md) | `sous-chef lint` in CI: SARIF, annotations, the baseline, exit codes |
| [docs/architecture.md](docs/architecture.md) | Module layout, contracts, configuration, conventions (for contributors) |
| [docs/bitbake-semantics.md](docs/bitbake-semantics.md) | The BitBake behaviour reproduced, with sources |
| [docs/differential.md](docs/differential.md) | Static answers compared with `bitbake -e`, every difference |
| [docs/performance.md](docs/performance.md) | Index, refresh, tool and hook timings |
| [docs/live-environment.md](docs/live-environment.md), [docs/live-verification.md](docs/live-verification.md) | The real Wrynose workspace sous-chef is checked against, and what was checked |
| [plugin/README.md](plugin/README.md) | The plugin's layout, decisions and the hook wire format |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Building, testing, the gate |

### License

MIT: see [LICENSE](LICENSE).

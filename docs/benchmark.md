# Whole-task benchmark (pilot)

The question: does an agent answering a question about a Yocto workspace use fewer tokens,
and answer as well or better, with sous-chef than without it? The README's "Token savings"
table compares one tool answer with the files it replaces. This measures **whole tasks**:
a real Claude Code session gets a question, uses whatever tools it has, and answers, once
without the plugin and once with it.

This is a **pilot**: 14 tasks, one session per task and condition, one model, one
workspace, read-only questions. One run per cell says nothing about significance. The
per-task numbers are reported, not only the averages, and a first run of the same tasks
(below) shows how much one cell moves between two runs.

Harness, tasks and reference facts: [bench/](../bench/README.md). Per-run numbers:
[bench/results/runs.tsv](../bench/results/runs.tsv); the tables below are
[bench/results/tables.md](../bench/results/tables.md).

A rerun on Sonnet 5.5 with the current code, tool search on, the `.gitignore` confound
removed and two runs per cell is at the end of this page ("Rerun on Sonnet 5.5 with the
current code").

## What was run

| | |
|---|---|
| Date | 2026-11-18 |
| Claude Code | 2.1.327 |
| Model | `--model haiku`, which the session reports as `claude-haiku-5-5` |
| sous-chef | release build of commit `05d7816` (the benchmark commits add only `bench/` and docs) |
| Workspace | `{{LIVE}}/yocto-kiss` (bootlin/yocto-kiss at `cc621c5`, kas; OE-core, meta-arm, meta-kiss, meta-kiss-ti; Wrynose; MACHINE `dogbonedark`, DISTRO `kiss`), the working directory of every session |
| Sessions | 28 task sessions + 2 overhead sessions + 1 sandbox probe (the measured pilot); before it, a first pilot of 30 sessions that was set aside (below), and 3 trial sessions |
| Limits | `--max-turns 25`, 300 s per session; no session reached either (largest: 22 turns, 74 s) |

### The two conditions

Both conditions, identical flags:

```
claude -p '<task prompt>' --model haiku --output-format stream-json --verbose
  --max-turns 25 --no-session-persistence --setting-sources ''
  --permission-mode dontAsk --tools Read,Grep,Glob,Bash,Skill
  --append-system-prompt 'Answer the question about this Yocto workspace. Do not modify any file. Do not run bitbake or kas.'
  --disallowedTools Edit Write NotebookEdit WebFetch WebSearch Agent Task
     mcp__plugin_sous-chef_sous-chef__scaffold mcp__plugin_sous-chef_sous-chef__bitbake
     'Bash(bitbake *)' 'Bash(kas *)' 'Bash(devtool *)' 'Bash(rm *)' 'Bash(mv *)' 'Bash(cp *)'
     'Bash(sed -i *)' 'Bash(find * -delete *)' 'Bash(find * -exec *)' 'Bash(git *)'
     'Bash(curl *)' 'Bash(wget *)'
     'Read(/{{LIVE}}/runs/**)' 'Read(/{{LIVE}}/bitbake-builds/**)' 'Read(/{{LIVE}}/scratch/**)'
     'Read(/{{LIVE}}/tools/**)' 'Read(/{{LIVE}}/venv/**)' 'Read(/{{LIVE}}/bitbake-setup-sources.json)'
  --allowedTools 'Bash(grep *)' 'Bash(rg *)' 'Bash(cat *)' 'Bash(ls *)' 'Bash(find *)'
     'Bash(head *)' 'Bash(tail *)' 'Bash(wc *)' 'Bash(sed -n *)'
```

- **A, baseline** adds `--strict-mcp-config`: no MCP server.
- **B, sous-chef** adds `--plugin-dir <repo>/plugin` (MCP server, hooks, skill) and allows
  the 14 read-only sous-chef tools (`recipe var deps search item overrides layers lint
  class status slice log diff audit`). `scaffold` and `bitbake` are denied, which also
  removes them from the context.

Environment of both: the parent session's `CLAUDE_CODE_*` variables removed,
`ENABLE_CLAUDEAI_MCP_SERVERS=false`, `XDG_CACHE_HOME` under the run directory, and
`{{LIVE}}/runs/x6-benchmark/wrap-bin` first on `PATH`.

**Isolation from the user's setup.** A scratch `CLAUDE_CONFIG_DIR` was not used: it needs a
new login or an `ANTHROPIC_API_KEY`, and neither was available (the user's credentials
were not copied). Instead `--setting-sources ''` loads no user, project or local settings
file, so the user's enabled plugin (an LSP plugin), hooks and permission rules are not
loaded; the workspace has no `CLAUDE.md` or `.claude/`. The account's claude.ai connectors
are not settings: in the first trial they appeared in condition B (21 Google Drive and Claude Docs
tools, 13,466 tokens) and were switched off with `ENABLE_CLAUDEAI_MCP_SERVERS=false`.
Condition B cannot use `--strict-mcp-config`, because that flag also drops a plugin's
server. Checked in every session's `init` event: A has the tools `Bash Glob Grep Read
Skill` and no MCP server; B has the same plus the 14 sous-chef tools and one MCP server,
`plugin:sous-chef:sous-chef` (connected). Both have Claude Code's built-in plugins and its
17 built-in skills; B adds the skill `sous-chef:yocto-layers`.

**Read-only.** `dontAsk` denies every call that would need approval. Read, Grep and Glob
are not in the allow list, so they work inside the working directory and are denied
outside it; the allowed Bash commands are also denied on paths outside it (the sandbox
probe below). The allow list holds read-only commands only; compound commands need every
part allowed, redirections are writes and are denied, `git` is denied. Edit, Write and
NotebookEdit are not available at all. The probe (`python3 bench/run.py --only containment
--conditions A`) asked a session to read `../runs/` with Read, Grep, Glob and three Bash
commands, run `cat /etc/hostname` and `ls ../`, and write `/tmp/x6-containment-test.txt`:
every outside access was denied, the read of `build/conf/bblayers.conf` worked, the file was
not written.

**sous-chef's configuration.** The plugin starts `sous-chef serve` in the project
directory, where yocto-kiss has no `sous-chef.toml`. Nothing was added to the checkout:
`wrap-bin/sous-chef` is a wrapper that runs the release binary with `--config
{{LIVE}}/runs/x6-benchmark/sous-chef.toml`, which sets the build directory, the two kas files
and `[bitbake] enabled = false`, so neither the `bitbake` tool nor `var
authoritative=true` could start BitBake. The hooks run through the same wrapper.

**The index** was built once before the sessions (`sous-chef index`; the first build took
1119 ms, 1755 files) and was warm for every B session, as it would be for a user after the
first tool call; that first build is not in the token or time numbers.

### The tasks

References were written by hand from the files and, for 11 of the 14, from BitBake's own
answer: the `bitbake -e` dumps in `{{LIVE}}/runs/differential/env/` and
`bitbake-layers show-layers` / `show-appends` of the kas build. Each task has 3 to 5 atomic
facts and a list of what would be wrong (`bench/tasks.json`).

| Task | Category | Question (short) | Reference from |
|---|---|---|---|
| t01-curl-srcuri | provenance | final `SRC_URI` of curl, which file gives each entry | curl.env |
| t02-providers | override | `PREFERRED_PROVIDER` of kernel and bootloader, where, which recipe | virtual_kernel.env |
| t03-linux-kiss-appends | structure | bbappends of linux-kiss, where defconfig and patch come from | show-appends, linux-kiss.env |
| t04-dropbear-classes | structure | classes dropbear inherits, directly and indirectly | dropbear.env (`BBINCLUDED`, `INHERIT`) |
| t05-curl-deps | dependencies | curl's build dependencies, what `PACKAGECONFIG[gnutls]` adds | curl.env |
| t06-gnutls-search | search (control) | where `PACKAGECONFIG[gnutls]` is set | grep |
| t07-busybox-install | task composition | what busybox `do_install` consists of, with appends and postfuncs | busybox.env |
| t08-layers | layers | enabled layers, collections, priorities, release | show-layers |
| t09-lint-meta-kiss | lint | what is wrong with meta-kiss by Yocto conventions | by hand (insane.bbclass, the recipes) |
| t10-linux-kiss-srcuri | python (uncertain) | the kernel URL, computed by inline python | linux-kiss.env |
| t11-distro-features | python (uncertain) | final `DISTRO_FEATURES`, with a python `:append` | dropbear.env |
| t12-kiss-image-install | override | final `IMAGE_INSTALL` of kiss-image | kiss-image.env |
| t13-git-srcrevs | search (control) | which meta-kiss recipes fetch from git, at which SRCREV | the two recipes |
| t14-u-boot-kiss | provenance | final `SRC_URI` of u-boot-kiss, overridden assignments, bbappends | u-boot-kiss.env, show-appends |

### Scoring

I scored every answer myself from `scoring/blind.md`, where each task's two answers appear
in shuffled order under labels `S01`..`S28` with the condition hidden (the key is in a
separate file, read after marking). Each reference fact is marked present (1), half (0.5:
partly right or incomplete), absent (0) or contradicted (-1); the score is the sum divided
by the number of facts. Wrong claims outside the facts are quoted in the marks, and an
answer that states something wrong or uncertain as certain is flagged as overconfident.
Blinding was imperfect: B answers often cite `layer:path:line` pointers and mention the
static evaluator, which gives the condition away. The marks are in
`{{LIVE}}/runs/x6-benchmark/scoring/marks.json`; the answers are not committed (they contain
local paths).

## Results

Tokens are the session's reported `usage`: input + output + cache creation + cache read.
Almost all of it is cache read (the conversation re-read on every turn), so a session's
total grows with turns times context size. Cost is Claude Code's own client-side estimate
(`total_cost_usd`, list prices), not a bill.

| Task | Total A | Total B | A/B | Turns A / B | Calls A / B | Wall A / B s | Score A | Score B |
|---|---|---|---|---|---|---|---|---|
| t01-curl-srcuri | 258,476 | 113,525 | 2.28 | 15 / 9 | 14 / 8 | 60.4 / 28.9 | 0.80 | 0.90 |
| t02-providers | 145,418 | 71,668 | 2.03 | 11 / 7 | 10 / 6 | 33.4 / 11.6 | 0.90 | 0.90 |
| t03-linux-kiss-appends | 120,473 | 171,667 | 0.70 | 22 / 14 | 21 / 13 | 36.0 / 23.0 | 0.75 | 1.00 |
| t04-dropbear-classes | 159,893 | 94,385 | 1.69 | 15 / 6 | 14 / 5 | 40.8 / 19.9 | 0.60 | 0.70 |
| t05-curl-deps | 84,639 | 150,350 | 0.56 | 12 / 13 | 11 / 12 | 25.4 / 27.2 | 0.10 | 0.70 |
| t06-gnutls-search (control) | 187,336 | 75,549 | 2.48 | 17 / 7 | 16 / 6 | 37.5 / 16.3 | -0.17 | 0.83 |
| t07-busybox-install | 294,010 | 116,457 | 2.52 | 19 / 12 | 18 / 11 | 52.8 / 33.2 | 0.60 | 1.00 |
| t08-layers | 84,412 | 43,761 | 1.93 | 13 / 3 | 12 / 2 | 20.9 / 9.4 | 1.00 | 0.75 |
| t09-lint-meta-kiss | 132,926 | 97,210 | 1.37 | 18 / 8 | 17 / 7 | 74.1 / 25.5 | 0.25 | 0.12 |
| t10-linux-kiss-srcuri | 72,780 | 104,876 | 0.69 | 10 / 8 | 9 / 7 | 30.9 / 15.3 | 1.00 | 1.00 |
| t11-distro-features | 103,762 | 184,492 | 0.56 | 12 / 14 | 11 / 13 | 28.2 / 28.2 | 0.12 | 1.00 |
| t12-kiss-image-install | 182,427 | 74,508 | 2.45 | 22 / 7 | 21 / 6 | 44.5 / 16.6 | 1.00 | 1.00 |
| t13-git-srcrevs (control) | 30,596 | 132,358 | 0.23 | 3 / 13 | 2 / 12 | 10.3 / 22.6 | 1.00 | 1.00 |
| t14-u-boot-kiss | 62,262 | 74,411 | 0.84 | 10 / 7 | 9 / 6 | 21.5 / 17.8 | 0.50 | 0.75 |

| Measure | A | B |
|---|---|---|
| Total tokens, sum of 14 tasks | 1,919,410 | 1,505,217 |
| Total tokens, median per task | 126,700 | 101,043 |
| Output tokens, sum | 85,117 | 42,849 |
| Turns, sum (median) | 199 (14) | 128 (8) |
| Tool calls, sum (median) | 185 (13) | 114 (7) |
| Wall time, sum (median) | 517 s (34.7 s) | 296 s (21.3 s) |
| Reported cost, sum | $0.099 | $0.058 |
| Score, mean (median) | 0.60 (0.68) | 0.83 (0.90) |
| Answers with a wrong claim | 7 of 14 | 3 of 14 |
| Answers flagged overconfident | 1 | 1 |

- **Tokens.** Over the 14 tasks, A used 1.28 times as many tokens as B (sum over sum). Per
  task the ratio A/B has a median of 1.53 and ranges from 0.23 to 2.52: B used fewer
  tokens on 8 tasks (ratio 1.37 to 2.52) and more on 6 (0.23 to 0.84).
- **The token split** (in `bench/results/tables.md`): input tokens proper are 4 to 20 per
  session; cache read is 74% to 98% of each session's total (87% over all of them). B wrote less to the cache than A on 9 of
  14 tasks and produced fewer output tokens on 12 of 14 (42,849 against 85,117 in all).
- **Correctness.** B scored higher on 8 tasks, equal on 4, lower on 2 (t08, t09). The
  largest differences are on tasks where A could not find OE-core (next point): t05 0.10
  against 0.70, t06 -0.17 against 0.83, t11 0.12 against 1.00.

### Why the baseline missed files

`yocto-kiss/.gitignore` lists `bitbake/`, `openembedded-core/` and `meta-*/` (kas clones
them into the project). Claude Code's Grep and Glob honour `.gitignore`, so a search from
the workspace root finds nothing in OE-core or meta-arm. In condition A:

- t05 (curl's dependencies): "The curl recipe isn't in it ... That layer isn't checked out
  in this workspace" (it is); the answer fell back on general knowledge. Score 0.10.
- t06 (the search control): "There is one `PACKAGECONFIG[gnutls]` assignment in the enabled
  layers" (there are 8); the session noticed that "a workspace-wide search ... returned
  nothing, even though the vte recipe matches". Score -0.17.
- t11: "The OE-core source isn't in the workspace", so the answer missed the two appends
  that come from OE-core files. Score 0.12.

Other A sessions found OE-core by naming paths inside it or by `find`/`grep` through Bash,
which does not read `.gitignore`. This is an effect of Claude Code's tools on a common kas
layout, not of Yocto knowledge; sous-chef reads `bblayers.conf` and is not affected. It
explains a large part of the score difference, and on the two search controls it is the
reason the "no advantage" expectation did not hold for t06.

### Where sous-chef helped

- **Provenance, override and composition questions**: t01, t02, t04, t07, t12 took 1.7 to
  2.5 times fewer tokens with B, with equal or better scores. In each, B answered with 5 to 11
  sous-chef calls (`var`, `recipe`, `item`, `search`) and no file read, where A read the recipe, the
  machine and distro configuration and several classes (t07: A 18 calls, 294k tokens; B 11
  calls, 116k tokens and the only answer that named both `do_install` postfuncs).
- **Layers** (t08): 2 `layers` calls, 3 turns, 43,761 tokens against 13 turns and 84,412.
  B's score is lower (0.75 against 1.00) because it did not say that meta-kiss-amd, -nxp and
  -st exist but are not enabled.

### Where it did not help, or was worse

- **t13, the second search control**: A answered in 3 turns and 30,596 tokens with two
  `grep`/`cat` commands; B made 12 sous-chef calls (7 `search`, 2 `layers`, 2 `var`,
  1 `recipe`) over 13 turns, 132,358 tokens. Same answer, 4.3 times the tokens.
- **t03, t05, t10, t11, t14**: B used 1.2 to 1.8 times more tokens than A. In t05 and t11 it
  answered much better (A could not see OE-core), in t03 and t14 somewhat better, in t10
  equally well. B kept calling `var` and `item` to confirm values after a first answer (t05:
  12 calls; t11: 13 calls, including 2 Grep).
- **t09, lint**: B ran the `lint` tool, which reports one info finding (no `HOMEPAGE` in
  linux-kiss), and relayed it as the whole answer (score 0.12; A 0.25). See below.

### Wrong or overconfident answers with sous-chef

- **t09 (overconfident)**: "Nothing in `meta-kiss` breaks Yocto conventions. The only
  finding is one informational lint hit, plus a few judgment calls." The layer has at least
  three more issues a reviewer would raise (an obsolete `${SRCPV}` in `sl`'s `PV`, no `PV`
  in `u-boot-kiss_git.bb`, an `INSANE_SKIP` of `ldflags`); sous-chef's lint has no rule for
  them, and the agent took the tool's silence as a clean bill. The first pilot's B answer
  to the same task said the same ("I found no hard Yocto-convention violations").
- **t04**: "the empty slots are the unset `USER_CLASSES` and `INHERIT_DISTRO`", next to an
  `INHERIT` value whose classes come from `INHERIT_DISTRO`. A small error; the class list
  itself was right.
- **t06**: listed vte under `recipes-extended` (it is `recipes-support`); the 8 hits were
  right.
- **Python-dependent values were not stated as certain.** In t10 B gave the resolved URL
  and said "the `${@...}` expression for the major version is not expanded in the static
  view. My reading is that it resolves to `7`"; in t11 it said "the static evaluator ...
  does not run the Python functions ... I derived the expansion by reading
  `filter_default_features`". Both were right.

### What the agent used, and what it did not

- **B's calls**, all 14 tasks: `var` 32, `search` 24, `recipe` 19, `item` 14, `layers` 12,
  `deps` 7, `lint`, `slice`, `status` 1 each; built-ins: Grep 2, Glob 1. Never used:
  `overrides`, `class`, `diff`, `audit`, `log`; the `Skill` tool was not called (the
  skill's description is in the context; its body was never loaded); Read and Bash never.
- **Reading files after a sous-chef answer**: in 12 of 14 B sessions every call was a
  sous-chef call. t09 made one Glob after `lint`, t11 two Greps after `var`. B never used
  Read: when it needed text it used `item`.
- **Hooks**: the 4 Grep calls of B (the first pilot included) were each handled by the
  hook in 0 ms with nothing to add (`hook.log`). The Bash and edit hooks had nothing to do
  in read-only sessions without Bash greps.
- **A's calls**: Read 70, Grep 61, Bash 32, Glob 30. Eleven A sessions had a Bash command
  denied (mostly `git ls-files`, denied by the rules; a few compound commands); B had none.

### The plugin's fixed overhead

The prompt "Say ok." once per condition (one turn, no tool):

| | Total tokens | Cost |
|---|---|---|
| A | 11,323 | $0.00012 |
| B | 20,679 | $0.00021 |

**9,356 tokens** more per turn of context: the 14 sous-chef tool definitions, the server's
instructions and the skill's entry in the skill list. Every B turn re-reads them (from the
cache), so a 10-turn B session carries about 94,000 tokens of overhead in its total; that
is why B is more expensive than A on short tasks such as t13 and t14. (In the first trial,
with the claude.ai connectors still loaded, B's start was 34,145 tokens.)
Re-measured after the x7 cuts, with and without tool search: "Plugin overhead after the
definition cuts" at the end of this page.

### Run to run variation: the first pilot

The same 14 tasks were run once before, identically except that Read, Grep and Glob were
allowed everywhere. Two A sessions then read files outside the workspace: t10 grepped
`{{LIVE}}/runs/differential/env/linux-kiss.env` (a reference dump) and t11 read
`{{LIVE}}/runs/bitbake-e-curl.out` and the bitbake-setup copy of OE-core. That run was set
aside, the sandbox was tightened (above) and the whole pilot was run again; its answers
were not scored. Its token totals (`bench/results/pilot1-tokens.tsv`) show how much one cell
moves:

| Task | A/B, first run | A/B, measured run |
|---|---|---|
| t01 | 2.33 | 2.28 |
| t02 | 1.55 | 2.03 |
| t03 | 0.51 | 0.70 |
| t04 | 3.87 | 1.69 |
| t05 | 1.10 | 0.56 |
| t06 | 1.33 | 2.48 |
| t07 | 3.43 | 2.52 |
| t08 | 1.96 | 1.93 |
| t09 | 0.68 | 1.37 |
| t10 | 1.03 | 0.69 |
| t11 | 1.26 | 0.56 |
| t12 | 1.29 | 2.45 |
| t13 | 0.34 | 0.23 |
| t14 | 1.88 | 0.84 |
| Sum A / sum B | 1.26 (2,306,817 / 1,823,751) | 1.28 (1,919,410 / 1,505,217) |

The overall ratio was the same in both runs (1.26 and 1.28), but single tasks moved by a
factor of up to 2.3 (t04, t14), and five tasks changed sides of 1.0 between runs (t05,
t09, t10, t11, t14). Below 1.0 in both runs: t03 and t13. Above 1.0 in both: t01, t02, t04, t06, t07, t08,
t12 (1.29 to 3.87).

## Cost of the pilot

| | Sessions | Total tokens | Reported cost |
|---|---|---|---|
| Measured run (28 tasks, 2 overhead, 1 probe) | 31 | 3,484,022 | $0.161 |
| First run (set aside) | 30 | 4,162,570 | $0.203 |
| Trials | 3 | about 54,000 | $0.010 |
| **All** | **64** | **about 7.70 million** | **$0.374** |

The check after the first two sessions (t01, both conditions: $0.012 and $0.005) put the
whole pilot well under the $10 limit, so it went ahead; the limit in tokens (3 million)
applies only when no cost is reported. Of the 7.7 million tokens, 86% are cache reads.

## Workspace after the pilot

`git status --porcelain` before and after, in `yocto-kiss`, `openembedded-core`, `meta-arm`
and `bitbake`: unchanged (`?? kas/kiss.lock.yaml` in yocto-kiss, written by
`scripts/live-env.sh` before this work; the other three clean). No BitBake or kas command
was issued by any session: every Bash call is in the transcripts, `git` and the BitBake and
kas commands were denied by rule, and sous-chef had BitBake switched off. One file under
`yocto-kiss` changed during the pilot: `bitbake/lib/bb/pysh/pyshtables.py` (gitignored, a
parser table BitBake regenerates when it starts), at 17:53:12, during the first run's
t05-A, whose calls were Read, Grep, Glob and one `cat` loop. Other agents were running live
tests that start BitBake in the same directory at that time; I attribute it to them. The
sessions wrote only under `{{LIVE}}/runs/x6-benchmark/` (transcripts, the sous-chef index
and handles) and whatever Claude Code itself keeps in its configuration directory;
`--no-session-persistence` keeps transcripts out of it.

## What this pilot does not show

- **One run per cell.** The first run shows single tasks moving by a factor of 2 between
  runs. The overall direction (A used about 1.3 times B's tokens, and B answered better)
  held twice; no per-task difference is established, and no significance is claimed.
- **One model.** Haiku 5.5 only. A larger model may read fewer files in A, use the tools
  differently in B, or check a lint result instead of trusting it.
- **Read-only questions only.** No editing task, no build, no log digestion: the hooks for
  Bash output and edits, `scaffold`, `bitbake` and `log` were not exercised.
- **One workspace**, small (two product layers, 1755 indexed files), with a `.gitignore`
  that hides the upstream layers from Claude Code's Grep and Glob. On a workspace without
  that, the baseline would find OE-core directly and the correctness gap would likely be
  smaller.
- **Self-scored**, by the agent that wrote the references, with imperfect blinding.
- **BitBake was off** for sous-chef, so `var authoritative=true` could not be tried on the
  python-dependent values.
- The index was warm; the first build (about 1.1 s) is not included.

A proper benchmark would add: at least 5 repetitions per cell, a second, larger model, a
judge other than the author of the references (or two scorers), editing tasks in a
scratch layer of a copied workspace (where the edit and lint hooks and `scaffold` matter),
log-digestion tasks from captured failures, and a second workspace (meta-openembedded
scale, without the `.gitignore` effect).

## Repeating or extending it

The run directory needs the release binary, the wrapper, the configuration and an index
(`bench/README.md`, "Running it"). Then, from the repository:

```sh
python3 bench/run.py --dry-run                 # the exact commands
python3 bench/run.py --overhead                # all tasks in both conditions (skips stored runs)
python3 bench/run.py --only containment --conditions A   # the sandbox probe
python3 bench/run.py --model sonnet --run-dir $SOUS_CHEF_LIVE/runs/x6-sonnet   # another model
python3 bench/score.py blind                   # then mark scoring/marks.json
python3 bench/score.py compute
```

## Plugin overhead after the definition cuts (2026-11-18)

Since the pilot, the `tools/list` payload went from 31,539 to 14,102 bytes, the skill from
about 8.0 KB to 2.5 KB, and the six core tools (`recipe var overrides item search class`)
carry `_meta["anthropic/alwaysLoad"]: true`, so that Claude Code's tool search defers the
other twelve. The pilot's sessions left `ToolSearch` out of `--tools`, which makes Claude
Code load every MCP tool upfront (code.claude.com/docs/en/mcp, "Configure tool search").
This re-measures the overhead with tool search on (its default) and off.

### Method

The prompt "Say ok.", one turn, no tool call, with the pilot's flags and environment (above:
`--model haiku`, `--setting-sources ''`, `--permission-mode dontAsk`,
`--no-session-persistence`, the same `--disallowedTools` and Bash allow list,
`ENABLE_CLAUDEAI_MCP_SERVERS=false`, the parent's `CLAUDE_CODE_*` variables removed,
`ENABLE_TOOL_SEARCH` unset, a `PATH` wrapper that runs the release build of `a3a310f` with
`--config` and `[bitbake] enabled = false`), plus `--debug-file` for each session. The
differences from the pilot:

- The working directory was an empty scratch directory, not a git checkout and not the
  yocto-kiss workspace (a build was running there). Nothing in the plugin's context
  depends on the workspace: the server instructions and tool definitions are fixed text.
  The baseline's absolute size differs from the pilot's (11,074 against 11,323 tokens); the
  differences between conditions are what is measured.
- The plugin conditions allow the 16 read-only sous-chef tools (the pilot's 14 plus `sigs`
  and `pkgdata`, added since); `scaffold` and `bitbake` are denied by name, which removes
  them from the context, as in the pilot.

| Session | `--tools` | Plugin |
|---|---|---|
| 1. Baseline | `Read,Grep,Glob,Bash,Skill,ToolSearch` | no (`--strict-mcp-config`) |
| 1'. Baseline as in the pilot | `Read,Grep,Glob,Bash,Skill` | no (`--strict-mcp-config`) |
| 2. Plugin, tool search on | `Read,Grep,Glob,Bash,Skill,ToolSearch` | `--plugin-dir plugin` |
| 3. Plugin, tool search off (the pilot's B) | `Read,Grep,Glob,Bash,Skill` | `--plugin-dir plugin` |

### Numbers

Claude Code 2.1.327, `--model haiku` (reported as `claude-haiku-5-5`). Total is the pilot's
sum, input + output + cache creation + cache read of the one turn; output was 4 tokens and
input 2 in every session, so input + cache creation + cache read is 4 less in each row and
gives the same differences.

| Session | Total | Cache creation / read | Tools in `init` | sous-chef upfront / deferred | Cost |
|---|---|---|---|---|---|
| 1. Baseline | 11,074 | 0 / 11,068 | 6 (5 built-in + `ToolSearch`) | – | $0.00011 |
| 1'. Baseline, no `ToolSearch` | 11,074 | 5,816 / 5,252 | 5 | – | $0.00122 |
| 2. Plugin, tool search on | 14,768 | 14,762 / 0 | 22 (6 built-in + 16 sous-chef) | 6 / 10 | $0.00295 |
| 3. Plugin, tool search off | 16,767 | 16,761 / 0 | 21 (5 built-in + 16 sous-chef) | 16 / 0 | $0.00335 |

- **Overhead with tool search on: 3,694 tokens** (2 − 1).
- **Overhead with tool search off: 5,693 tokens** (3 − 1'), against the pilot's 9,356 in the
  same setup: 39% less.
- Tool search saves 1,999 tokens more (3 − 2): the ten deferred definitions are replaced
  by their names and the `ToolSearch` tool.

**Upfront and deferred**, from each session's debug log. Session 2: "Dynamic tool loading:
0/10 deferred tools included", that is, the ten tools without `alwaysLoad` (`layers deps
lint log slice status audit diff sigs pkgdata`) deferred and the six core tools sent
upfront. Session 3: "Tool search disabled: ToolSearchTool is not available", "0/0
deferred tools": all 16 upfront. Session 1: "Tool search disabled: no deferred tools
available to search"; `init` lists `ToolSearch`, but with no MCP tool to defer the
baseline came out the same size as 1'. The `init` event lists every tool, deferred or
not, so the split comes from the log, not from `init`. What session 2 adds, by the
documentation and the log: the six core definitions, the server instructions (732
characters, which name the deferred tools), the deferred tools' names, the skill's entry
in the skill list (`sous-chef:yocto-layers`, 18 skills against 17) and the `ToolSearch`
tool; the measurement does not split the 3,694 tokens between them.

**Caching.** The pilot's two sessions read everything from the cache; here the plugin
sessions wrote it (cache creation, 1-hour writes, priced above cache reads), so the costs
are higher than the pilot's and are not comparable with them; the token totals are.

**A repeat.** The first baseline session (1, tool search at its default) came out at 11,114
tokens: Claude Code's built-in `cc-plugin-easel` plugin loaded in that session only (none
of the other four), and it adds a prompt section. That session was set aside and run
again; the repeat, without that plugin, is the row above. Five sessions in all, $0.0089
reported.

## Rerun on Sonnet 5.5 with the current code (2026-11-20)

The pilot's 14 tasks again, with a larger model, the current sous-chef, tool search at
Claude Code's default, the `.gitignore` confound removed and two sessions per task and
condition. The pilot's numbers above are unchanged. Per-run numbers:
[bench/results/rerun-runs.tsv](../bench/results/rerun-runs.tsv); full tables:
[bench/results/rerun-tables.md](../bench/results/rerun-tables.md).

### What was run

| | |
|---|---|
| Date | 2026-11-20 |
| Claude Code | 2.1.328 |
| Model | `--model sonnet`, which every session reports as `claude-sonnet-5-5` |
| sous-chef | release build of commit `2bd568c` (the rerun commits add only `bench/` and docs) |
| Workspace | the pilot's: `{{LIVE}}/yocto-kiss`, Wrynose, MACHINE `dogbonedark`, DISTRO `kiss`; index of 1755 files, warm |
| Sessions | 56 task sessions (14 tasks x 2 conditions x 2 runs), 4 overhead sessions ("Say ok.", 2 per condition), 4 probes (below) |
| Limits | `--max-turns 25`, 300 s, `--max-budget-usd 3` per session; none reached (largest: 16 turns, 61.6 s, $0.16) |
| Command | `python3 bench/run.py --model sonnet --tool-search --no-vcs-ignore --repeats 2 --max-budget-usd 3 --cost-cap 30 --overhead --run-dir {{LIVE}}/runs/bench-rerun` |

Everything else is the pilot's setup ("The two conditions" above): the same prompts, system
prompt append, `--setting-sources ''`, `--permission-mode dontAsk`,
`--no-session-persistence`, deny and allow lists, `ENABLE_CLAUDEAI_MCP_SERVERS=false`, the
parent's `CLAUDE_CODE_*` variables removed, `--strict-mcp-config` in A, `--plugin-dir
plugin` in B, and a `PATH` wrapper that runs the release binary with `--config
{{LIVE}}/runs/bench-rerun/sous-chef.toml` (`[bitbake] enabled = false`, the two kas files,
the build directory). The differences:

- **Model**: Sonnet 5.5 instead of Haiku 5.5.
- **Tool search on** in both conditions: `--tools Read,Grep,Glob,Bash,Skill,ToolSearch`,
  `ENABLE_TOOL_SEARCH` unset. The pilot left `ToolSearch` out, which loads every MCP tool
  upfront. Here the six `alwaysLoad` tools are sent upfront and the other ten deferred
  ("Plugin overhead after the definition cuts" above). `init` lists 6 tools in A and 22 in
  B (6 built-in and 16 sous-chef) in every session, and one MCP server in B,
  `plugin:sous-chef:sous-chef`, connected in all 28 B sessions. The built-in plugins were
  the same four in every session (no `cc-plugin-easel`).
- **16 read-only sous-chef tools allowed** in B, the pilot's 14 plus `sigs` and `pkgdata`;
  `scaffold` and `bitbake` denied as before.
- **The `.gitignore` confound removed** (next section).
- **Two runs per cell**, run ids `-r1` and `-r2`, in the order r1 for every task, then r2.

### The `.gitignore` confound, and what was changed

`yocto-kiss/.gitignore` lists `bitbake/`, `openembedded-core/`, `build/` and `meta-*/`
(with `!meta-kiss*/`). Claude Code's Grep runs ripgrep, which skips gitignored files, and
has no setting to change that (code.claude.com/docs/en/tools-reference, "Grep tool
behavior"); Glob does not honour `.gitignore` by default (same page, "Glob tool
behavior"). The first probe, with the pilot's setup (condition A, Haiku: "call Grep once
for `PACKAGECONFIG\[gnutls\]` with no path"), got `No files found`.

The change, in both conditions (`bench/run.py --no-vcs-ignore`):

- `USE_BUILTIN_RIPGREP=0`, so Claude Code runs the `rg` on `PATH` instead of its bundled
  one;
- `{{LIVE}}/runs/bench-rerun/rg-bin/rg` first on `PATH`: a wrapper that logs each call and
  runs `/usr/bin/rg` (ripgrep 15.2.0) with `--no-ignore-vcs --ignore-file
  {{LIVE}}/runs/bench-rerun/rg-ignore` before the arguments Claude Code passed;
- `rg-ignore` holds `/build/*` and `!/build/conf/`: the build directory (14 GB, with
  BitBake's output) stays out of searches as it was in the pilot, except `build/conf`,
  which sous-chef reads too.

Nothing was added to the workspace. With the wrapper the same probe returned the 8 files
in `openembedded-core/meta`. The log has 325 calls over the run: Grep searches, Glob's file
listings and the agents' own `rg` commands all went through it. Side effects: Glob, which
Claude Code runs as `rg --files --no-ignore`, now also skips `build/*` except `build/conf`
(`--no-ignore` does not cancel an explicit `--ignore-file`), where in the pilot it could
list the build directory; and an `rg` command run through Bash behaves like Grep. Both
conditions now search the same files: every layer in the checkout, `build/conf`, nothing
else of `build/`.

The probes: the ignore probe before the fix and with a first version of the wrapper
(Haiku, $0.004 together), the final ignore probe (Haiku, $0.0005), and the pilot's
containment probe on Sonnet with the new flags ($0.045): every access outside the
workspace was denied, `build/conf/bblayers.conf` was read, `/tmp/x6-containment-test.txt`
was not written.

**The workspace during the run.** Another agent was rebuilding `curl` and `zlib` in the same
build directory. Before the run, from 14:51 to 14:59, its kas configuration had added
`../scratch/meta-scratch` to `build/conf/bblayers.conf` (the containment probe saw it); the
run started after the file was back to the five layers, and a watcher hashed
`bblayers.conf` and `local.conf` every 3 s from 15:00:04 to the end of the run: unchanged.
The last BitBake server in `build/bitbake-cookerdaemon.log` started at 15:00:07 (the other
agent's kas checkout) and exited at 15:00:13, as the first session started; none after.

### Results

Tokens are the pilot's sum (input + output + cache creation + cache read); cost is Claude
Code's client-side estimate (`total_cost_usd`). Each cell is the mean of the two runs, with
the two values (r1, r2) in brackets where they differ.

| Task | Total A | Total B | A/B r1 / r2 | Turns A / B | Calls A / B | Score A | Score B |
|---|---|---|---|---|---|---|---|
| t01-curl-srcuri | 74,841 (64,658, 85,024) | 54,908 (54,330, 55,487) | 1.19 / 1.53 | 8 / 5 | 7 / 4 | 0.90 | 0.90 |
| t02-providers | 142,946 (143,046, 142,846) | 65,704 (54,839, 76,570) | 2.61 / 1.87 | 15.5 / 6.5 | 14.5 / 5.5 | 1.00 | 0.90 |
| t03-linux-kiss-appends | 50,180 (49,557, 50,803) | 90,594 (90,970, 90,217) | 0.54 / 0.56 | 11.5 / 10.5 | 10.5 / 9.5 | 0.75 | 1.00 |
| t04-dropbear-classes | 95,206 (68,265, 122,147) | 47,305 (47,717, 46,893) | 1.43 / 2.60 | 13 / 4.5 | 12 / 3.5 | 0.95 (0.90, 1.00) | 0.90 |
| t05-curl-deps | 89,175 (88,221, 90,129) | 83,568 (83,680, 83,456) | 1.05 / 1.08 | 13 / 9 | 12 / 8 | 0.80 | 0.80 |
| t06-gnutls-search (control) | 34,330 (27,128, 41,533) | 30,182 (30,187, 30,178) | 0.90 / 1.38 | 3.5 / 2 | 2.5 / 1 | 1.00 | 1.00 |
| t07-busybox-install | 136,094 (95,276, 176,913) | 100,548 (83,350, 117,746) | 1.14 / 1.50 | 13.5 / 11 | 12.5 / 10 | 0.60 | 0.80 (1.00, 0.60) |
| t08-layers | 53,084 (65,607, 40,560) | 45,682 (45,726, 45,639) | 1.43 / 0.89 | 8 / 4 | 7 / 3 | 1.00 | 0.75 |
| t09-lint-meta-kiss | 54,008 (53,788, 54,229) | 60,968 (52,024, 69,913) | 1.03 / 0.78 | 12.5 / 10.5 | 11.5 / 9.5 | 0.62 | 1.00 |
| t10-linux-kiss-srcuri | 51,134 (51,095, 51,172) | 49,819 (50,374, 49,264) | 1.01 / 1.04 | 8 / 4.5 | 7 / 3.5 | 1.00 | 0.83 |
| t11-distro-features | 98,552 (77,904, 119,201) | 80,368 (110,409, 50,326) | 0.71 / 2.37 | 11 / 7.5 | 10 / 6.5 | 0.88 | 1.00 |
| t12-kiss-image-install | 85,404 (101,801, 69,007) | 40,198 (31,736, 48,660) | 3.21 / 1.42 | 10 / 3.5 | 9 / 2.5 | 1.00 | 1.00 |
| t13-git-srcrevs (control) | 40,110 (33,816, 46,405) | 45,847 (45,791, 45,903) | 0.74 / 1.01 | 4 / 5 | 3 / 4 | 1.00 | 1.00 |
| t14-u-boot-kiss | 65,680 (66,410, 64,951) | 56,826 (47,500, 66,151) | 1.40 / 0.98 | 9.5 / 5.5 | 8.5 / 4.5 | 0.38 | 0.75 |

Wall time and cost per task, with their ranges: `bench/results/rerun-tables.md`. Sums over
the 14 tasks, per run, and the mean of the two runs (score: mean over the tasks):

| Measure | A r1 | A r2 | B r1 | B r2 | A mean | B mean | A / B |
|---|---|---|---|---|---|---|---|
| Total tokens | 986,572 | 1,154,920 | 828,633 | 876,403 | 1,070,746 | 852,518 | 1.26 |
| Output tokens | 37,379 | 39,660 | 20,809 | 21,365 | 38,520 | 21,087 | 1.83 |
| Turns | 135 | 147 | 91 | 87 | 141 | 89 | 1.58 |
| Tool calls | 121 | 133 | 77 | 73 | 127 | 75 | 1.69 |
| Wall time, s | 388 | 421 | 232 | 241 | 404 | 236 | 1.71 |
| Reported cost, $ | 1.06 | 0.94 | 0.85 | 0.57 | 1.00 | 0.71 | 1.41 |
| Score, mean | 0.84 | 0.85 | 0.92 | 0.89 | 0.85 | 0.90 | |
| Answers with a wrong claim | 4 of 14 | 4 of 14 | 1 of 14 | 1 of 14 | | | |
| Answers flagged overconfident | 0 | 0 | 0 | 0 | | | |

- **Tokens.** A used 1.26 times B's tokens (sum of the means; 1.19 in r1, 1.32 in r2), the
  same as the pilot's 1.28 with a different model and setup. Per task the ratio of the
  means has a median of 1.16 and ranges from 0.55 (t03) to 2.18 (t02): B used fewer tokens
  on 11 of 14 tasks. Below 1.0 in both runs: t03 only. Above 1.0 in both: t01, t02, t04,
  t05, t07, t10, t12. Six tasks changed sides of 1.0 between the two runs (t06, t08, t09,
  t11, t13, t14), and one cell moved by a factor of 2.2 between runs (t11 B: 110,409 and
  50,326), so per-task ratios below about 1.5 are within what two runs of one cell show.
- **Score.** B 0.90, A 0.85 (pilot: 0.83 and 0.60). The baseline's gain is mostly the
  confound fix: the pilot's three A answers that could not find OE-core (t05 0.10, t06
  -0.17, t11 0.12) are 0.80, 1.00 and 0.88 here. B scored higher on 5 tasks (t03, t07,
  t09, t11, t14), equal on 5, lower on 4 (t02, t04, t08, t10, each by an omitted detail,
  below). The two runs of a cell got the same score in 26 of 28 cells.
- **Turns, calls, time, cost.** B took fewer turns and tool calls on 13 of 14 tasks (not
  t13), 236 s against 404 s of wall time and $0.71 against $1.00 per run of the 14 tasks.
- **Cache.** Cache reads are 84% of the A sessions' tokens and 85% of the B sessions'.

### Wrong or overconfident answers

No answer was flagged overconfident. Every answer with a wrong claim:

**Baseline (8 answers)**

- t07, both runs: missed both `do_install` postfuncs. r1: "The only extra function is
  `do_install_ptest`"; r2: "there are no extra install functions".
- t09, both runs: "`u-boot-kiss_git.bb` also has no `SUMMARY`" (it gets one from OE-core's
  `u-boot.inc`). r1 also: "the default (systemd) ... is poky's default and not oe-core's,
  which is `none`" (OE-core's `defaultsetup.conf` has `INIT_MANAGER ??= "systemd"`). Both
  missed `u-boot-kiss`'s missing `PV` and `linux-kiss`'s missing `HOMEPAGE`.
- t11 r1: final value without the second `sysvinit`, and "Only the first eight features are
  listed in `kiss.conf`" (all nine are). t11 r2: "`sysvinit` appears once".
- t14, both runs: listed the `u-boot-kiss_%.bbappend` files of meta-kiss-amd, -nxp and
  -st as bbappends that apply (r1: "Yes, three of them"), with their machine-specific
  `SRC_URI` appends in a table; those layers are not enabled. Neither run read
  `bblayers.conf`.

**sous-chef (2 answers)**

- **t07 r2**: missed both postfuncs and said the inherited classes "don't change
  `do_install` itself". The tool output that led there: `var` with `name=do_install
  recipe=busybox` returned `final: function do_install, 1 piece in order:
  meta:recipes-core/busybox/busybox.inc:225-375` and nothing about the task's
  `[postfuncs]` flag. The r1 session asked `var` for `name=do_install[postfuncs]` and got
  `" remove_libtool_la rm_systemd_unitdir"`, the right answer (score 1.00). A product bug:
  `var` on a task function does not mention its `prefuncs` / `postfuncs` unless they are
  asked for by name (or with `flags=true`). The same session also called the tool by a
  short name (`item`: "No such tool available") and tried to Read a path under a
  non-existent `layers/` directory.
- **t11 r1**: "BitBake de-duplicates the trailing `sysvinit`" (it does not). The raw value
  it gave was right, and so was the tool's: `var DISTRO_FEATURES` printed `final:
  "sysvinit ... ldconfig sysvinit ${@oe.utils.filter_default_features('DISTRO_FEATURES',
  d)}"` with the python marked as not evaluated. The wrong claim is the model's, not a
  tool output's.

Omissions without a wrong claim, where B scored lower than A: t08, both B runs relayed the
`layers` tool's five enabled layers and did not say that meta-kiss-amd, -nxp and -st exist
but are not enabled (the pilot's B answer missed the same); t02, both B runs gave
u-boot-kiss's version as `git` without the v2026.04 of its `SRCREV`; t10, both B runs took
`KERNELORG_MIRROR` from the expanded URL instead of naming `bitbake.conf`; t04, both B runs
listed the global `INHERIT` classes without the classes they inherit (and said so), where
the A r2 answer expanded them. t14 B did not state the `SRCREV` in either run.

The pilot's sous-chef failure on t09 is gone: both B runs relayed the `lint` findings
(`linux-kiss` without `SUMMARY` and `HOMEPAGE`, `${SRCPV}` in `sl`, the `INSANE_SKIP` of
`ldflags`, `u-boot-kiss` without `PV`) and scored 1.00, where both A runs read every file
in the layer and scored 0.62.

### What the agent used

- **B, all 28 sessions**: `var` 43, `recipe` 26, `search` 23, `deps` 6, `ToolSearch` 6,
  `item` 5, `layers` 5, `class` 2, `lint` 2; built-ins Read 17, Glob 6, Bash 6, Grep 3.
  `ToolSearch` loaded a deferred tool before its first use each time one was needed (t05
  `deps`, t08 `layers`, t09 `lint`, both runs). Never used: `overrides`, `slice`,
  `status`, `log`, `diff`, `audit`, `sigs`, `pkgdata`; the `Skill` tool was not called.
- **B used built-in tools** in 10 of 28 sessions and Read in 5, unlike the pilot's B (no
  Read): t09 read 5 to 7 files after `lint` to look for more findings, t03 r1 read three
  files, t07 r2 and t11 r2 one each.
- **A, all 28 sessions**: Grep 84, Read 81, Glob 45, Bash 44. 22 A sessions and 3 B
  sessions had one Bash command denied, nearly all compound commands (`cd ...;`, `for`
  loops) and two `git` commands; the agents went on with Read and Grep.

### Fixed overhead

"Say ok.", two sessions per condition: A 10,525 tokens, B 14,219 tokens in all four,
**3,694 tokens** more for the plugin with tool search on, the same as the Haiku
measurement above.

### Cost

| | Sessions | Total tokens | Reported cost |
|---|---|---|---|
| Task sessions | 56 | 3,846,528 | $3.43 |
| Overhead sessions | 4 | 49,488 | $0.035 |
| Probes | 4 | 95,442 | $0.050 |
| **All** | **64** | **3,991,458** | **$3.51** |

The first four task sessions (t01 and t02, both conditions) cost $0.32, which projected
about $4.50 for the 56; the cap was $30, so the run went ahead with two runs per cell.

### Scoring and the rubric

Scored as in the pilot: `bench/score.py blind` shuffled the 56 answers with the condition
hidden (labels `S01`..`S56`, grouped by task), I marked every fact, then joined the key
(`BENCH_RUN_DIR=... bench/score.py compute-repeats`). Blinding was imperfect for the same
reason as before (B cites `layer:path:line` and the static evaluator). Marks:
`{{LIVE}}/runs/bench-rerun/scoring/marks.json`.

No reference fact was stale, and the rubric was used as it stands: before scoring I
checked the facts that could have moved against the workspace files (curl 8.19.0 and its
patches, the meta-kiss recipes and their `SRCREV`, `PV`, `SUMMARY`, `HOMEPAGE` and
`INSANE_SKIP` lines, the dogbonedark machine file, `kiss.conf`), and sous-chef's changes
since the pilot do not touch what the facts say about the workspace. Two judgement calls,
applied to both conditions: an answer missing one item of a list fact (for example
`virtual/update-alternatives` in t05, or `sanity` in t04) gets a half mark; an answer that
says the postfuncs do not exist gets an absent mark with the claim quoted as wrong, while
listing the unenabled layers' bbappends as applying (t14) is the opposite of the fact and
is marked contradicted.

### Workspace and isolation after the rerun

- BitBake was switched off in sous-chef's configuration for every session (`[bitbake]
  enabled = false`), `bitbake` and `scaffold` were denied by name, and `Bash(bitbake *)`,
  `Bash(kas *)` and `Bash(git *)` were denied; no session ran `bitbake` or `kas` (every
  call is in the transcripts).
- Outside `build/`, the only files under `yocto-kiss` newer than the run's start marker
  are the `.git` index, `HEAD` and `config` of `bitbake`, `meta-arm` and
  `openembedded-core`, at 15:00:07: the other agent's kas checkout, before the first
  session. In `build/`: `conf/bblayers.conf` and `conf/local.conf` (same content,
  rewritten at 15:00:07), `bitbake-cookerdaemon.log` (last written at 15:00:13), `cache/`
  and `sstate-cache/`, all from the other agent.
- The sessions wrote only under `{{LIVE}}/runs/bench-rerun/` (transcripts, the sous-chef
  index, the `rg` call log).

### What the rerun does not show

- **Two runs per cell** show how much a cell moves, not significance. Six of 14 tasks
  changed sides of 1.0 between the two runs; the overall ratio was 1.19 and 1.32.
- **Not a model comparison.** Haiku in the pilot and Sonnet here, with other changes in
  between (tool search, the confound fix, the sous-chef version).
- Read-only questions, one small workspace, self-scored, BitBake off: as in the pilot.

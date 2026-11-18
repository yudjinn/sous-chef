# Whole-task benchmark

Does an agent answering a question about a Yocto workspace use fewer tokens, and answer as
well, with sous-chef than without it? Each task of `tasks.json` is run as a real,
read-only Claude Code session (`claude -p`), once per condition:

- **A, baseline**: Claude Code's built-in Read, Grep, Glob, Bash (an allowlist of read-only
  commands) and Skill tools; no plugin and no MCP server.
- **B, sous-chef**: the same, plus the plugin (`--plugin-dir plugin`: the MCP server, the
  hooks and the skill).

Results and method: [docs/benchmark.md](../docs/benchmark.md).

## Files

| File | What it is |
|---|---|
| `tasks.json` | The tasks: id, category, prompt, the reference facts an answer must contain, what would be wrong, and where the reference comes from (mostly real `bitbake -e` dumps) |
| `run.py` | Runs the sessions, stores each raw stream-json transcript under the run directory, appends one summary line per run to `<run dir>/runs.jsonl`; resumable, `--dry-run`, `--limit`, `--only`, `--show RUN_ID`, `--table` |
| `score.py` | `blind`: every answer shuffled, condition hidden, next to its task's facts (`<run dir>/scoring/blind.md`, `key.json`, a `marks.json` template); `compute`: scores from the marks, and the tables in `results/` |
| `results/` | `runs.tsv` (one row per session: tokens, turns, tool calls, wall time, marks) and `tables.md` (per-task comparison, totals and medians, overhead). No transcript or answer text: those stay in the run directory |

## Running it

Prerequisites: the live workspace (`docs/live-environment.md`), a logged-in `claude`, and
a run directory prepared once:

```sh
RUN=$SOUS_CHEF_LIVE/runs/x6-benchmark
cargo build --release
mkdir -p $RUN/bin $RUN/wrap-bin $RUN/cache
cp target/release/sous-chef $RUN/bin/
cat > $RUN/sous-chef.toml <<EOF
kas = ["$SOUS_CHEF_LIVE/yocto-kiss/kas/kiss.yaml", "$SOUS_CHEF_LIVE/yocto-kiss/kas/dogbonedark.yaml"]
build_dir = "$SOUS_CHEF_LIVE/yocto-kiss/build"
[bitbake]
enabled = false
EOF
printf '#!/bin/sh\nexec %s/bin/sous-chef --config %s/sous-chef.toml "$@"\n' $RUN $RUN > $RUN/wrap-bin/sous-chef
chmod +x $RUN/wrap-bin/sous-chef
(cd $SOUS_CHEF_LIVE/yocto-kiss && PATH=$RUN/wrap-bin:$PATH XDG_CACHE_HOME=$RUN/cache sous-chef index)
```

The wrapper gives the plugin's `sous-chef serve` and `sous-chef hook` the benchmark's
configuration (BitBake switched off) without adding a file to the workspace checkout.

Then:

```sh
python3 bench/run.py --dry-run            # the exact commands
python3 bench/run.py --overhead           # every task in both conditions, plus 'Say ok.'
python3 bench/run.py --only t05-curl-deps --conditions B
python3 bench/score.py blind              # then fill <run dir>/scoring/marks.json
python3 bench/score.py compute
```

Every session runs with the workspace as working directory, `--setting-sources ""` (no
user, project or local settings: no user plugins, hooks or permissions), `--permission-mode
dontAsk` (anything not allowed is denied, never prompted), `--no-session-persistence`,
`ENABLE_CLAUDEAI_MCP_SERVERS=false`, `XDG_CACHE_HOME` under the run directory, and the run
directory's `wrap-bin` first on `PATH`. Condition A adds `--strict-mcp-config` (no MCP
server); B cannot, because that flag also drops a plugin's server.

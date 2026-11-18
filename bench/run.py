#!/usr/bin/env python3
"""Whole-task benchmark harness: each task of bench/tasks.json run as a real, read-only
Claude Code session (`claude -p`), once without sous-chef (condition A) and once with the
plugin (condition B).

Each run is a fresh session (`--no-session-persistence`), sequential, with a turn limit and
a timeout. The raw stream-json transcript of every run is stored under the run directory
(never in the repository: transcripts contain local paths), and one summary line per run
is appended to <run dir>/runs.jsonl. A run whose summary exists is skipped (resumable).

Usage:
    bench/run.py [--dry-run] [--limit N] [--only ID[,ID...]] [--conditions A,B]
                 [--model haiku] [--max-turns 25] [--timeout 300] [--overhead]
    bench/run.py --show RUN_ID          # print what a stored run did
    bench/run.py --table                # per-run table of the stored summaries

Environment:
    SOUS_CHEF_LIVE   the live directory (default ~/Development/sous-chef-live); the
                     workspace is $SOUS_CHEF_LIVE/yocto-kiss, the run directory
                     $SOUS_CHEF_LIVE/runs/x6-benchmark (override with --run-dir).

The run directory must hold wrap-bin/sous-chef (a wrapper that runs the benchmark's
binary with --config <run dir>/sous-chef.toml) and cache/ (XDG_CACHE_HOME); see
bench/README.md for how they are made.
"""
import argparse
import collections
import json
import os
import shlex
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LIVE = os.environ.get("SOUS_CHEF_LIVE", os.path.expanduser("~/Development/sous-chef-live"))

SYSTEM_APPEND = (
    "Answer the question about this Yocto workspace. Do not modify any file. "
    "Do not run bitbake or kas."
)

# Built-in tools offered in both conditions. Grep and Glob are named explicitly (on Linux
# they are not in the default set); no Edit, Write, NotebookEdit, WebFetch, WebSearch,
# Agent or anything else.
TOOLS = "Read,Grep,Glob,Bash,Skill"

# Bash commands that run without a prompt. Under --permission-mode dontAsk anything else
# (and anything Claude Code does not classify as read-only) is denied. Compound commands
# need every part allowed; redirection targets are checked as writes (denied).
BASH_ALLOW = [
    "Bash(grep *)", "Bash(rg *)", "Bash(cat *)", "Bash(ls *)", "Bash(find *)",
    "Bash(head *)", "Bash(tail *)", "Bash(wc *)", "Bash(sed -n *)",
]
ALLOW_COMMON = ["Read", "Grep", "Glob"] + BASH_ALLOW
# sous-chef's read-only tools (condition B). scaffold (writes) and bitbake (runs BitBake)
# are denied below, and BitBake is also switched off in the benchmark's sous-chef.toml.
SC = "mcp__plugin_sous-chef_sous-chef__"
SC_READ_ONLY = ["recipe", "var", "deps", "search", "item", "overrides", "layers", "lint",
                "class", "status", "slice", "log", "diff", "audit"]
ALLOW_B = [SC + t for t in SC_READ_ONLY]
DENY = [
    "Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch", "Agent", "Task",
    SC + "scaffold", SC + "bitbake",
    "Bash(bitbake *)", "Bash(kas *)", "Bash(devtool *)", "Bash(rm *)", "Bash(mv *)",
    "Bash(cp *)", "Bash(sed -i *)", "Bash(find * -delete *)", "Bash(find * -exec *)",
    "Bash(git *)", "Bash(curl *)", "Bash(wget *)",
]

# Environment variables of a parent Claude Code session that must not leak into the child.
PARENT_ENV_PREFIXES = ("CLAUDE_CODE_", "CLAUDECODE", "CLAUDE_PID", "CLAUDE_JOB_DIR",
                       "CLAUDE_EFFORT", "CLAUDE_AGENT")


def load_tasks():
    with open(os.path.join(REPO, "bench", "tasks.json")) as f:
        return json.load(f)


def command(prompt, cond, opts):
    argv = [
        "claude", "-p", prompt,
        "--model", opts.model,
        "--output-format", "stream-json", "--verbose",
        "--max-turns", str(opts.max_turns),
        "--no-session-persistence",
        "--setting-sources", "",
        "--permission-mode", "dontAsk",
        "--tools", TOOLS,
        "--append-system-prompt", SYSTEM_APPEND,
        "--disallowedTools", *DENY,
        "--allowedTools", *(ALLOW_COMMON + (ALLOW_B if cond == "B" else [])),
    ]
    if cond == "A":
        # No MCP server at all.
        argv += ["--strict-mcp-config"]
    else:
        argv += ["--plugin-dir", os.path.join(REPO, "plugin")]
    return argv


def environment(opts):
    env = {k: v for k, v in os.environ.items() if not k.startswith(PARENT_ENV_PREFIXES)}
    env["PATH"] = os.path.join(opts.run_dir, "wrap-bin") + os.pathsep + env.get("PATH", "")
    env["XDG_CACHE_HOME"] = os.path.join(opts.run_dir, "cache")
    # The account's claude.ai connectors are not settings and are not dropped by
    # --setting-sources; condition B cannot use --strict-mcp-config (it would drop the
    # plugin's server too), so switch them off in both conditions.
    env["ENABLE_CLAUDEAI_MCP_SERVERS"] = "false"
    return env


def parse_stream(path):
    """Summary of one stored stream-json transcript."""
    init, result = None, None
    tools = collections.Counter()
    calls = []
    denied = 0
    texts = []
    if not os.path.exists(path):
        return None
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                e = json.loads(line)
            except json.JSONDecodeError:
                continue
            t = e.get("type")
            if t == "system" and e.get("subtype") == "init":
                init = e
            elif t == "assistant":
                for c in (e.get("message") or {}).get("content") or []:
                    if c.get("type") == "tool_use":
                        tools[c.get("name")] += 1
                        calls.append({"name": c.get("name"), "input": c.get("input")})
                    elif c.get("type") == "text":
                        texts.append(c.get("text", ""))
            elif t == "user":
                for c in (e.get("message") or {}).get("content") or []:
                    if isinstance(c, dict) and c.get("type") == "tool_result" and c.get("is_error"):
                        denied += 1
            elif t == "result":
                result = e
    return {"init": init, "result": result, "tools": tools, "calls": calls,
            "tool_errors": denied, "texts": texts}


def summarize(run_id, task_id, cond, meta, parsed):
    res = (parsed or {}).get("result") or {}
    u = res.get("usage") or {}
    inp = u.get("input_tokens", 0)
    out = u.get("output_tokens", 0)
    cc = u.get("cache_creation_input_tokens", 0)
    cr = u.get("cache_read_input_tokens", 0)
    tools = (parsed or {}).get("tools") or {}
    sc_calls = sum(n for name, n in tools.items() if name and name.startswith(SC))
    init = (parsed or {}).get("init") or {}
    return {
        "run_id": run_id, "task": task_id, "cond": cond,
        "exit": meta.get("exit"), "timed_out": meta.get("timed_out", False),
        "wall_s": round(meta.get("wall_s", 0), 1),
        "subtype": res.get("subtype"), "is_error": res.get("is_error"),
        "num_turns": res.get("num_turns"), "duration_ms": res.get("duration_ms"),
        "cost_usd": res.get("total_cost_usd"),
        "input": inp, "output": out, "cache_creation": cc, "cache_read": cr,
        "total": inp + out + cc + cr,
        "model_usage": res.get("modelUsage"),
        "tools": dict(tools), "tool_calls": sum(tools.values()),
        "sous_chef_calls": sc_calls, "builtin_calls": sum(tools.values()) - sc_calls,
        "tool_errors": (parsed or {}).get("tool_errors", 0),
        "permission_denials": len(res.get("permission_denials") or []),
        "init_tools": init.get("tools"), "init_mcp": init.get("mcp_servers"),
        "init_plugins": init.get("plugins"), "init_skills": init.get("skills"),
        "init_model": init.get("model"), "claude_version": init.get("claude_code_version"),
        "answer": res.get("result"),
    }


def run_one(run_id, task_id, cond, prompt, opts):
    raw = os.path.join(opts.run_dir, "raw")
    os.makedirs(raw, exist_ok=True)
    argv = command(prompt, cond, opts)
    if opts.dry_run:
        print(f"# {run_id}  (cwd {opts.workspace})")
        print(" ".join(shlex.quote(a) for a in argv))
        return None
    out_path = os.path.join(raw, run_id + ".jsonl")
    err_path = os.path.join(raw, run_id + ".stderr")
    print(f"[{time.strftime('%H:%M:%S')}] {run_id} ...", flush=True)
    t0 = time.monotonic()
    timed_out = False
    with open(out_path, "w") as out, open(err_path, "w") as err:
        try:
            p = subprocess.run(argv, cwd=opts.workspace, env=environment(opts),
                               stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                               timeout=opts.timeout)
            code = p.returncode
        except subprocess.TimeoutExpired:
            code, timed_out = None, True
    meta = {"exit": code, "timed_out": timed_out, "wall_s": time.monotonic() - t0,
            "argv": argv, "cwd": opts.workspace}
    with open(os.path.join(raw, run_id + ".meta.json"), "w") as f:
        json.dump(meta, f, indent=1)
    s = summarize(run_id, task_id, cond, meta, parse_stream(out_path))
    with open(os.path.join(opts.run_dir, "runs.jsonl"), "a") as f:
        f.write(json.dumps(s) + "\n")
    print(f"    exit={code} turns={s['num_turns']} total={s['total']} cost={s['cost_usd']} "
          f"tools={s['tools']} wall={s['wall_s']}s", flush=True)
    return s


def stored(run_dir):
    done = {}
    p = os.path.join(run_dir, "runs.jsonl")
    if os.path.exists(p):
        with open(p) as f:
            for line in f:
                if line.strip():
                    s = json.loads(line)
                    done[s["run_id"]] = s
    return done


def show(run_id, opts):
    parsed = parse_stream(os.path.join(opts.run_dir, "raw", run_id + ".jsonl"))
    if not parsed:
        sys.exit(f"no stored run {run_id}")
    init = parsed["init"] or {}
    for k in ("model", "claude_code_version", "tools", "mcp_servers", "plugins", "skills",
              "permissionMode", "cwd"):
        print(f"{k}: {init.get(k)}")
    for c in parsed["calls"]:
        print("CALL", c["name"], json.dumps(c["input"])[:300])
    res = parsed["result"] or {}
    print("RESULT", json.dumps({k: v for k, v in res.items() if k != "result"}, indent=1))
    print("ANSWER\n" + (res.get("result") or ""))


def table(opts):
    rows = sorted(stored(opts.run_dir).values(), key=lambda s: (s["task"], s["cond"]))
    cols = ["run_id", "exit", "num_turns", "total", "input", "output", "cache_creation",
            "cache_read", "cost_usd", "tool_calls", "sous_chef_calls", "wall_s"]
    print("\t".join(cols))
    for s in rows:
        print("\t".join(str(s.get(c)) for c in cols))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--dry-run", action="store_true", help="print the commands, run nothing")
    ap.add_argument("--limit", type=int, help="run at most N sessions (after skipping stored ones)")
    ap.add_argument("--only", help="comma-separated task ids (or 'overhead')")
    ap.add_argument("--conditions", default="A,B")
    ap.add_argument("--overhead", action="store_true",
                    help="also run the trivial overhead prompt once per condition")
    ap.add_argument("--model", default="haiku")
    ap.add_argument("--max-turns", type=int, default=25)
    ap.add_argument("--timeout", type=int, default=300, help="seconds per session")
    ap.add_argument("--run-dir", default=os.path.join(LIVE, "runs", "x6-benchmark"))
    ap.add_argument("--workspace", default=os.path.join(LIVE, "yocto-kiss"))
    ap.add_argument("--show", metavar="RUN_ID")
    ap.add_argument("--table", action="store_true")
    opts = ap.parse_args()
    if opts.show:
        return show(opts.show, opts)
    if opts.table:
        return table(opts)

    spec = load_tasks()
    jobs = []
    if opts.overhead or (opts.only and "overhead" in opts.only.split(",")):
        jobs.append(("overhead", spec["overhead_prompt"]))
    for t in spec["tasks"]:
        if opts.only and t["id"] not in opts.only.split(","):
            continue
        jobs.append((t["id"], t["prompt"]))
    conds = opts.conditions.split(",")
    done = stored(opts.run_dir)
    n = 0
    for task_id, prompt in jobs:
        for cond in conds:
            run_id = f"{task_id}-{cond}"
            if run_id in done and not opts.dry_run:
                continue
            if opts.limit is not None and n >= opts.limit:
                return
            run_one(run_id, task_id, cond, prompt, opts)
            n += 1


if __name__ == "__main__":
    main()

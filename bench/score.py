#!/usr/bin/env python3
"""Scoring and result tables for the whole-task benchmark.

    bench/score.py blind      # write <run dir>/scoring/blind.md: every answer, shuffled,
                              # condition hidden, with its task's reference facts; and
                              # key.json (label -> run) and a marks.json template
    bench/score.py compute    # join marks.json with runs.jsonl and the raw transcripts;
                              # write bench/results/*.tsv and bench/results/tables.md

Marks (filled in by hand, from blind.md only): for each label, one mark per reference
fact, "P" (present), "H" (half: partly right or incomplete), "A" (absent) or
"C" (contradicted: the answer states the opposite or a wrong value); plus "wrong" (wrong
claims not covered by the facts, quoted), "overconfident" (true when the answer states as
certain something that is uncertain or wrong without saying so) and "notes".

Score of a run = (P + 0.5 H - C) / number of facts. The same rubric for both conditions.
"""
import collections
import json
import os
import random
import statistics
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import run as harness  # noqa: E402

REPO = harness.REPO
RUN_DIR = os.path.join(harness.LIVE, "runs", "x6-benchmark")
SCORING = os.path.join(RUN_DIR, "scoring")
RESULTS = os.path.join(REPO, "bench", "results")
SC = harness.SC


def tasks():
    return {t["id"]: t for t in harness.load_tasks()["tasks"]}


def blind():
    runs = [s for s in harness.stored(RUN_DIR).values() if s["task"] not in ("overhead", "containment")]
    rng = random.Random(20261118)
    rng.shuffle(runs)
    os.makedirs(SCORING, exist_ok=True)
    key, out, template = {}, [], {}
    ts = tasks()
    # Group by task so each task's answers are scored side by side, order inside shuffled.
    by_task = collections.defaultdict(list)
    for s in runs:
        by_task[s["task"]].append(s)
    n = 0
    for tid in sorted(by_task):
        t = ts[tid]
        out.append(f"# {tid} ({t['category']})\n\n**Prompt.** {t['prompt']}\n")
        out.append("**Facts.**\n" + "\n".join(f"{i+1}. {f}" for i, f in enumerate(t["facts"])))
        out.append("\n**Wrong if.**\n" + "\n".join(f"- {w}" for w in t["wrong"]) + "\n")
        for s in by_task[tid]:
            n += 1
            label = f"S{n:02d}"
            key[label] = s["run_id"]
            template[label] = {"task": tid, "facts": ["?"] * len(t["facts"]), "wrong": [],
                               "overconfident": False, "notes": ""}
            answer = (s.get("answer") or "(no answer)").replace(harness.LIVE, "{{LIVE}}")
            out.append(f"## {label}\n\n{answer}\n")
    with open(os.path.join(SCORING, "blind.md"), "w") as f:
        f.write("\n".join(out))
    with open(os.path.join(SCORING, "key.json"), "w") as f:
        json.dump(key, f, indent=1)
    marks = os.path.join(SCORING, "marks.json")
    if not os.path.exists(marks):
        with open(marks, "w") as f:
            json.dump(template, f, indent=1)
    print(f"{n} answers in {SCORING}/blind.md")


def score_of(m):
    v = {"P": 1.0, "H": 0.5, "A": 0.0, "C": -1.0}
    return sum(v[x] for x in m["facts"]) / len(m["facts"])


def call_pattern(run_id):
    """Built-in calls made after the first sous-chef call, and the sequence of names."""
    p = harness.parse_stream(os.path.join(RUN_DIR, "raw", run_id + ".jsonl"))
    names = [c["name"] for c in (p or {}).get("calls", [])]
    after = 0
    seen = False
    for n in names:
        if n.startswith(SC):
            seen = True
        elif seen:
            after += 1
    short = [n[len(SC):] if n.startswith(SC) else n for n in names]
    return after, short


def fmt_ratio(a, b):
    return f"{a / b:.2f}" if b else "-"


def compute():
    with open(os.path.join(SCORING, "marks.json")) as f:
        marks = json.load(f)
    with open(os.path.join(SCORING, "key.json")) as f:
        key = json.load(f)
    runs = harness.stored(RUN_DIR)
    ts = tasks()
    by_run = {}
    for label, run_id in key.items():
        m = marks[label]
        if "?" in m["facts"]:
            sys.exit(f"{label} is not fully marked")
        by_run[run_id] = dict(m, label=label, score=score_of(m))
    os.makedirs(RESULTS, exist_ok=True)

    cols = ["task", "cond", "total", "input", "output", "cache_creation", "cache_read",
            "cost_usd", "num_turns", "tool_calls", "sous_chef_calls", "builtin_calls",
            "builtin_after_sc", "tool_errors", "wall_s", "score", "facts", "wrong",
            "overconfident", "calls"]
    rows = []
    for s in sorted(runs.values(), key=lambda s: (s["task"], s["cond"])):
        r = {c: s.get(c) for c in cols}
        after, seq = call_pattern(s["run_id"])
        r["builtin_after_sc"] = after if s["cond"] == "B" else ""
        r["calls"] = " ".join(seq)
        m = by_run.get(s["run_id"])
        if m:
            r["score"] = round(m["score"], 2)
            r["facts"] = "".join(m["facts"])
            r["wrong"] = len(m["wrong"])
            r["overconfident"] = m["overconfident"]
        else:
            r["score"] = r["facts"] = r["wrong"] = r["overconfident"] = ""
        rows.append(r)
    with open(os.path.join(RESULTS, "runs.tsv"), "w") as f:
        f.write("\t".join(cols) + "\n")
        for r in rows:
            f.write("\t".join("" if r[c] is None else str(r[c]) for c in cols) + "\n")

    # Per-task comparison table (markdown).
    cell = {(r["task"], r["cond"]): r for r in rows}
    lines = ["| Task | Category | Total A | Total B | A/B | In+out A | In+out B | Cache read A | "
             "Cache read B | Cache write A | Cache write B | Turns A | Turns B | Calls A | "
             "Calls B (sous-chef) | Wall A s | Wall B s | Score A | Score B |",
             "|" + "---|" * 19]
    agg = collections.defaultdict(list)
    for tid in sorted(ts):
        a, b = cell.get((tid, "A")), cell.get((tid, "B"))
        if not a or not b:
            continue
        lines.append(
            f"| {tid} | {ts[tid]['category']} | {a['total']:,} | {b['total']:,} | "
            f"{fmt_ratio(a['total'], b['total'])} | {a['input'] + a['output']:,} | "
            f"{b['input'] + b['output']:,} | {a['cache_read']:,} | {b['cache_read']:,} | "
            f"{a['cache_creation']:,} | {b['cache_creation']:,} | {a['num_turns']} | "
            f"{b['num_turns']} | {a['tool_calls']} | {b['tool_calls']} ({b['sous_chef_calls']}) | "
            f"{a['wall_s']} | {b['wall_s']} | {a['score']} | {b['score']} |")
        for k in ("total", "num_turns", "tool_calls", "wall_s", "cost_usd", "score", "output"):
            agg[(k, "A")].append(a[k])
            agg[(k, "B")].append(b[k])
        agg[("ratio", "-")].append(a["total"] / b["total"])
    lines.append("")
    lines.append("| Measure | A sum | B sum | A median | B median |")
    lines.append("|---|---|---|---|---|")
    for k in ("total", "output", "num_turns", "tool_calls", "wall_s", "cost_usd", "score"):
        av, bv = agg[(k, "A")], agg[(k, "B")]
        sa, sb = sum(av), sum(bv)
        def f(x):
            return f"{x:,.0f}" if abs(x) >= 1000 else f"{x:.4g}"
        lines.append(f"| {k} | {f(sa)} | {f(sb)} | {f(statistics.median(av))} | "
                     f"{f(statistics.median(bv))} |")
    rs = agg[("ratio", "-")]
    lines.append("")
    lines.append(f"Per-task A/B total-token ratio: median {statistics.median(rs):.2f}, "
                 f"min {min(rs):.2f}, max {max(rs):.2f}; sum A / sum B "
                 f"{sum(agg[('total', 'A')]) / sum(agg[('total', 'B')]):.2f}.")
    ov = {c: runs.get(f"overhead-{c}") for c in "AB"}
    if ov["A"] and ov["B"]:
        lines.append("")
        lines.append("| Overhead ('Say ok.') | Total | Cache write | Cache read | Cost USD |")
        lines.append("|---|---|---|---|---|")
        for c in "AB":
            o = ov[c]
            lines.append(f"| {c} | {o['total']:,} | {o['cache_creation']:,} | "
                         f"{o['cache_read']:,} | {o['cost_usd']} |")
        lines.append(f"\nFixed overhead of the plugin: {ov['B']['total'] - ov['A']['total']:,} "
                     "tokens per session start.")
    all_runs = list(runs.values())
    lines.append("")
    lines.append(f"Sessions: {len(all_runs)}; total tokens {sum(s['total'] for s in all_runs):,}; "
                 f"reported cost USD {sum(s['cost_usd'] or 0 for s in all_runs):.4f}.")
    sc_tools = collections.Counter()
    for s in all_runs:
        if s["cond"] == "B":
            for n, c in s["tools"].items():
                sc_tools[n[len(SC):] if n.startswith(SC) else n] += c
    lines.append("")
    lines.append("Tool calls in condition B, all tasks: " +
                 ", ".join(f"{n} {c}" for n, c in sc_tools.most_common()) + ".")
    a_tools = collections.Counter()
    for s in all_runs:
        if s["cond"] == "A":
            a_tools.update(s["tools"])
    lines.append("Tool calls in condition A, all tasks: " +
                 ", ".join(f"{n} {c}" for n, c in a_tools.most_common()) + ".")
    text = "\n".join(lines) + "\n"
    with open(os.path.join(RESULTS, "tables.md"), "w") as f:
        f.write(text.replace(harness.LIVE, "{{LIVE}}"))
    print(text)


if __name__ == "__main__":
    {"blind": blind, "compute": compute}[sys.argv[1] if len(sys.argv) > 1 else "compute"]()

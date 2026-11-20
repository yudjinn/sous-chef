#!/usr/bin/env python3
"""Draw the stratified random sample of lint findings for the precision measurement.

    lint-sample.py [--seed N] [--size N] [--exclude-layer L]... RELEASE=FILE.json...

Each FILE is the output of `sous-chef lint --all --format json` on the workspace of
RELEASE (docs/lint-rules.md, "Measured precision"). The findings of all files are pooled
and stratified by rule: every rule that fired gets min(3, its count) findings, and the
rest of SIZE is shared out in proportion to what is left of each rule's count (largest
remainder, ties by rule id), never more than a rule has. Within a rule the findings are
sorted (release, pointer, message) and drawn with `random.Random(seed).sample`, one
generator for the whole run, rules in sorted order, so the same inputs and seed give the
same sample.

Findings in a layer named by --exclude-layer are left out before sampling (the live
workspace's `meta-scratch` is a throwaway layer other checks rewrite).

Output on stdout: the TSV header of tests/fixtures/lint-precision/labels.tsv and one row
per sampled finding with the label and reason columns empty, then on stderr the strata.
"""

import argparse
import json
import random
import sys
from collections import defaultdict

HEADER = ["rule", "release", "pointer", "severity", "label", "reason", "message"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=20261120)
    ap.add_argument("--size", type=int, default=150)
    ap.add_argument("--min", type=int, default=3, dest="minimum")
    ap.add_argument("--exclude-layer", action="append", default=[])
    ap.add_argument("inputs", nargs="+", metavar="RELEASE=FILE")
    args = ap.parse_args()

    by_rule = defaultdict(list)
    for spec in args.inputs:
        release, _, path = spec.partition("=")
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
        for sev, rule, pointer, message in data["findings"]["rows"]:
            if pointer.split(":", 1)[0] in args.exclude_layer:
                continue
            by_rule[rule].append((release, pointer, message, sev))

    rules = sorted(by_rule)
    counts = {r: len(by_rule[r]) for r in rules}
    alloc = {r: min(args.minimum, counts[r]) for r in rules}
    left = args.size - sum(alloc.values())
    if left > 0:
        rest = {r: counts[r] - alloc[r] for r in rules}
        total = sum(rest.values())
        if total <= left:
            alloc = dict(counts)
        else:
            quota = {r: left * rest[r] / total for r in rules}
            for r in rules:
                alloc[r] += int(quota[r])
            remaining = args.size - sum(alloc.values())
            order = sorted(rules, key=lambda r: (-(quota[r] - int(quota[r])), r))
            for r in order:
                if remaining == 0:
                    break
                if alloc[r] < counts[r]:
                    alloc[r] += 1
                    remaining -= 1

    rng = random.Random(args.seed)
    out = sys.stdout
    out.write("\t".join(HEADER) + "\n")
    for r in rules:
        pool = sorted(by_rule[r])
        for release, pointer, message, sev in sorted(rng.sample(pool, alloc[r])):
            out.write("\t".join([r, release, pointer, sev, "", "", message]) + "\n")
        print(f"{r}\t{counts[r]}\t{alloc[r]}", file=sys.stderr)
    print(f"total\t{sum(counts.values())}\t{sum(alloc.values())}\tseed {args.seed}", file=sys.stderr)


if __name__ == "__main__":
    main()

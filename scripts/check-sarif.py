#!/usr/bin/env python3
"""Check a SARIF log from `sous-chef lint --format sarif` before it is uploaded.

    check-sarif.py SCHEMA SARIF [REPO_ROOT]

1. Validates SARIF against the SARIF 2.1.0 JSON schema SCHEMA (the OASIS file
   `sarif-2.1/schema/sarif-schema-2.1.0.json` from github.com/oasis-tcs/sarif-spec;
   not vendored here because of its licence). Needs the `jsonschema` package.
2. Checks what GitHub code scanning requires on top of the schema
   (docs.github.com, "SARIF support for code scanning"): the required properties, no
   empty required strings, the 1024-character description limits, the upload limits
   (20 runs per file, 25,000 results and rules per run, 1,000 locations per result,
   20 tags per rule), `partialFingerprints.primaryLocationLineHash` on every result,
   and every `artifactLocation.uri` relative, not absolute.
3. With REPO_ROOT: every result's file exists under REPO_ROOT, so code scanning can show
   it in the repository.

Exit 0 when everything holds, 1 with one line per problem otherwise. Used by
.github/workflows/lint-demo.yml; see docs/ci.md.
"""

import json
import os
import sys
import urllib.parse


def github_checks(log, root):
    problems = []

    def need_text(obj, key, where, limit=None):
        text = (obj.get(key) or {}).get("text")
        if not isinstance(text, str) or not text:
            problems.append(f"{where}: {key}.text missing or empty")
        elif limit and len(text) > limit:
            problems.append(f"{where}: {key}.text is {len(text)} characters (limit {limit})")

    runs = log.get("runs") or []
    if not 1 <= len(runs) <= 20:
        problems.append(f"{len(runs)} runs (GitHub accepts 1 to 20)")
    for r, run in enumerate(runs):
        driver = (run.get("tool") or {}).get("driver") or {}
        if not driver.get("name"):
            problems.append(f"run {r}: tool.driver.name missing")
        rules = driver.get("rules")
        if not isinstance(rules, list):
            problems.append(f"run {r}: tool.driver.rules missing")
            rules = []
        if len(rules) > 25000:
            problems.append(f"run {r}: {len(rules)} rules (limit 25,000)")
        for rule in rules:
            where = f"rule {rule.get('id')}"
            if not rule.get("id"):
                problems.append(f"run {r}: a rule without id")
            need_text(rule, "shortDescription", where, 1024)
            need_text(rule, "fullDescription", where, 1024)
            need_text(rule, "help", where)
            if len(rule.get("name") or "") > 255:
                problems.append(f"{where}: name over 255 characters")
            if len((rule.get("properties") or {}).get("tags") or []) > 20:
                problems.append(f"{where}: more than 20 tags")
        results = run.get("results")
        if not isinstance(results, list):
            problems.append(f"run {r}: results missing")
            results = []
        if len(results) > 25000:
            problems.append(f"run {r}: {len(results)} results (limit 25,000)")
        for i, res in enumerate(results):
            where = f"run {r} result {i} ({res.get('ruleId')})"
            need_text(res, "message", where)
            fp = (res.get("partialFingerprints") or {}).get("primaryLocationLineHash")
            if not isinstance(fp, str) or not fp:
                problems.append(f"{where}: partialFingerprints.primaryLocationLineHash missing")
            locs = res.get("locations") or []
            if not 1 <= len(locs) <= 1000:
                problems.append(f"{where}: {len(locs)} locations (1 to 1,000)")
            for loc in locs:
                phys = loc.get("physicalLocation") or {}
                uri = (phys.get("artifactLocation") or {}).get("uri") or ""
                if not uri:
                    problems.append(f"{where}: artifactLocation.uri missing")
                    continue
                if uri.startswith("/") or urllib.parse.urlsplit(uri).scheme:
                    problems.append(f"{where}: uri {uri!r} is not relative to the repository")
                    continue
                if not isinstance((phys.get("region") or {}).get("startLine"), int):
                    problems.append(f"{where}: region.startLine missing")
                if root is not None:
                    path = os.path.join(root, urllib.parse.unquote(uri))
                    if not os.path.isfile(path):
                        problems.append(f"{where}: {uri} is not a file under {root}")
    return problems


def main(argv):
    if len(argv) not in (3, 4):
        print(__doc__.strip().splitlines()[2].strip(), file=sys.stderr)
        return 2
    import jsonschema

    with open(argv[1], encoding="utf-8") as f:
        schema = json.load(f)
    with open(argv[2], encoding="utf-8") as f:
        log = json.load(f)
    root = argv[3] if len(argv) == 4 else None

    validator = jsonschema.Draft4Validator(schema)
    problems = [
        f"schema: {'/'.join(str(p) for p in e.absolute_path) or '(root)'}: {e.message}"
        for e in sorted(validator.iter_errors(log), key=lambda e: list(e.absolute_path))
    ]
    problems += github_checks(log, root)
    for p in problems:
        print(p)
    results = sum(len(run.get("results") or []) for run in log.get("runs") or [])
    rules = sum(
        len(((run.get("tool") or {}).get("driver") or {}).get("rules") or [])
        for run in log.get("runs") or []
    )
    status = "FAIL" if problems else "ok"
    print(f"{argv[2]}: {status}: SARIF 2.1.0 schema and GitHub checks, "
          f"{rules} rules, {results} results, {len(problems)} problems")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

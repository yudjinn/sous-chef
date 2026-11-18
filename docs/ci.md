# sous-chef in CI

`sous-chef lint` runs without a build environment: it needs the layers checked out (and
OE-core, or the release cannot be told; see "Release contract" in architecture.md), a
`sous-chef.toml` naming your writable layers, and nothing else. No BitBake, no kas, no
network. This page shows how to run it in a CI job, how to keep old findings from failing
new merge requests (the baseline), and what the exit codes mean.

The examples are documentation. The sous-chef repository has no remote and its own CI has
never run, so the GitHub Actions workflow below has **not been tested against a live
GitHub run**; the commands it runs are the ones the test suite runs (`tests/ci.rs`).

## Output formats

| `--format` | What | Where the summary goes |
|---|---|---|
| `text` (default) | summary line, `severity rule pointer message` rows, notes | stdout |
| `json` | `{summary, errors, warnings, infos, by_rule, findings}` | stdout |
| `sarif` | SARIF 2.1.0: one run, every rule of the catalogue (id, short and full description from docs/lint-rules.md, default level), one result per finding | stderr |
| `github` | one workflow command per finding: `::error file=F,line=A,endLine=B,title=RULE::MESSAGE` (`::warning`, `::notice` for info), escaped as `@actions/core` does | stderr |

File paths in `sarif` and `github` are relative to the repository root: the git top level
of the working directory (`git rev-parse --show-toplevel`), or the working directory when
it is not in a git checkout. SARIF uses `uriBaseId` `%SRCROOT%`. A file outside that root
(for example a layer checked out next to the repository) gets an absolute path.

Each finding has a **fingerprint**: a hash of the rule id, the file (`layer:path`) and the
text of the statement the finding is on, plus an occurrence number (`<hash>:<n>`). The line
number is not part of it, so inserting a line above a finding does not make it a new alert;
changing the statement does. SARIF carries it as `partialFingerprints.primaryLocationLineHash`
(the key GitHub code scanning uses) and `sousChefFinding/v1`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | no reported finding at or above `--fail-on` (default `error`); `--write-baseline` always 0 |
| 1 | a reported finding at or above `--fail-on` (with `--baseline`: a new finding) |
| 2 | lint could not run: bad target, unknown rule id, unreadable workspace, missing or malformed baseline file, `--migrate-to` with `--format sarif` / `github` |

`--severity` hides findings below a level; hidden findings never fail the run.

## The baseline

A layer with years of history has findings nobody will fix this week. Record them once,
commit the file, and fail only on new ones:

```sh
sous-chef lint --write-baseline .sous-chef-baseline.json   # exit 0; commit the file
sous-chef lint --baseline .sous-chef-baseline.json          # reports and fails on new findings only
```

The file is readable JSON, sorted, the same bytes for the same tree:

```json
{
  "schema": "sous-chef-lint-baseline/1",
  "findings": [
    { "file": "meta-mine:recipes-app/app/app_0.1.bb", "rule": "hygiene/release-syntax", "fingerprint": "…:1" }
  ]
}
```

With `--baseline` the report lists only findings not in the file, and a note says how many
baseline entries matched and how many are **stale** (no longer found: the finding was
fixed, or its statement changed). Refresh the file with `--write-baseline` when the stale
count grows. `--fail-on` applies to the new findings only. A baseline file that is missing,
is not JSON or has another `schema` is an error (exit 2), so a typo in the path cannot turn
the check off.

## GitHub Actions

The job checks out the layer repository and its dependencies, gets the `sous-chef`
binary, runs lint with SARIF output and uploads it to code scanning. Upload needs the
`security-events: write` permission. Build the binary from your copy of the sous-chef
source (below), or download one your organisation publishes.

```yaml
name: lint layers
on: [push, pull_request]

jobs:
  sous-chef:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      security-events: write
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive          # or a kas checkout step: `kas checkout kas/my.yaml`

      - name: Build sous-chef
        run: |
          git clone --depth 1 "$SOUS_CHEF_REPO" /tmp/sous-chef
          cargo build --release --manifest-path /tmp/sous-chef/Cargo.toml
          echo /tmp/sous-chef/target/release >> "$GITHUB_PATH"
        env:
          SOUS_CHEF_REPO: https://example.org/your/sous-chef.git   # your copy

      - name: Lint (SARIF)
        id: lint
        # Exit 1 means findings; keep going so they are uploaded, then fail below.
        run: |
          set +e
          sous-chef lint --format sarif --baseline .sous-chef-baseline.json > sous-chef.sarif
          echo "code=$?" >> "$GITHUB_OUTPUT"

      - uses: github/codeql-action/upload-sarif@v3
        if: always()
        with:
          sarif_file: sous-chef.sarif
          category: sous-chef

      - name: Fail on new findings
        if: steps.lint.outputs.code != '0'
        run: exit "${{ steps.lint.outputs.code }}"
```

Without code scanning (a private repository without GitHub Advanced Security, for example),
annotations show the findings on the pull request's diff instead:

```yaml
      - name: Lint (annotations)
        run: sous-chef lint --format github --baseline .sous-chef-baseline.json
```

The repository needs a `sous-chef.toml` at its root (or pass `--config`), for example:

```toml
build_dir = "build"                   # where conf/bblayers.conf is, if the job has one
writable_layers = ["meta-mine"]       # the layers this repository owns
```

Without a build directory, pass the layers with `--layer DIR` (repeatable).

## GitLab CI and other systems

Any CI that runs a shell can use the JSON output and the exit codes. GitLab, with the JSON
report kept as an artifact:

```yaml
sous-chef:
  stage: test
  script:
    - sous-chef lint --format json --baseline .sous-chef-baseline.json > sous-chef.json
  artifacts:
    when: always
    paths: [sous-chef.json]
```

The job fails when `sous-chef lint` exits 1 (a new finding at or above `--fail-on`) or 2
(lint could not run). To fail on warnings too, add `--fail-on warning`. `jq` reads the
counts: `jq '.errors, .warnings, .by_rule' sous-chef.json`.

## Migration reports in CI

`sous-chef lint --migrate-to <codename>` (docs/lint-rules.md, "Running lint") answers "what
breaks if this layer moves to release X". It exits 1 when a finding new with that release
is at or above `--fail-on`, so a scheduled job can watch a layer's readiness for the next
LTS:

```sh
sous-chef lint --migrate-to wrynose --format json > migration.json
```

It prints text or JSON only; SARIF and annotations are for ordinary lint runs.

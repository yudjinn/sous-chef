# sous-chef in CI

`sous-chef lint` runs without a build environment: it needs the layers checked out (and
OE-core, or the release cannot be told; see "Release contract" in architecture.md), a
`sous-chef.toml` naming your writable layers, and nothing else. No BitBake, no kas, no
network. This page shows how to run it in a CI job, how to keep old findings from failing
new merge requests (the baseline), and what the exit codes mean.

The commands on this page are the ones the test suite runs (`tests/ci.rs`), and this
repository's own [`lint-demo` workflow](../.github/workflows/lint-demo.yml) runs them on
GitHub on the fixture workspace: annotations, SARIF checked against the SARIF 2.1.0 schema
and uploaded to code scanning, and a baseline round trip ("This repository's demo job",
below).

## Output formats

| `--format` | What | Where the summary goes |
|---|---|---|
| `text` (default) | summary line, `severity rule pointer message` rows, notes | stdout |
| `json` | `{summary, errors, warnings, infos, by_rule, findings}`; the findings are `findings.rows` | stdout |
| `sarif` | SARIF 2.1.0: one run, every rule of the catalogue (id, short and full description from docs/lint-rules.md, help, default level, `properties.tags` and `properties.problem.severity`), one result per finding, at most 25,000 | stderr |
| `github` | one workflow command per finding: `::error file=F,line=A,endLine=B,title=RULE::MESSAGE` (`::warning`, `::notice` for info), escaped as `@actions/core` does | stderr |

File paths in `sarif` and `github` are relative to the repository root: the git top level
of the working directory (`git rev-parse --show-toplevel`), or the working directory when
it is not in a git checkout. Run lint from inside the checkout (any subdirectory, such as
the build directory's parent) and the paths are the ones code scanning and the pull
request diff use; no `checkout_path` is needed. SARIF uses `uriBaseId` `%SRCROOT%`. A file
outside that root (for example a layer checked out next to the repository) gets an
absolute path, and code scanning cannot show it in the repository.

GitHub's limits, and what sous-chef does about them:

- **Code scanning** rejects a SARIF run with more than 25,000 results. sous-chef writes the
  25,000 most severe findings and says on stderr how many it left out. GitHub shows the
  top 5,000 of a run. A baseline keeps the count down.
- **Annotations**: GitHub Actions shows at most 10 error, 10 warning and 10 notice
  annotations per step, and 50 per job ([actions/toolkit,
  problem-matchers.md](https://github.com/actions/toolkit/blob/main/docs/problem-matchers.md#limitations));
  the rest are dropped without a message. When a type has more than 10 findings,
  `--format github` prints one `::notice` before the findings saying how many there are;
  every finding is still a line in the step's log.

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

Where it lives and who updates it: keep it at the repository root (the examples use
`.sous-chef-baseline.json`), committed next to `sous-chef.toml`, and run lint from the same
directory locally and in CI, since the paths in the file are `layer:path`. Refresh it on a
branch, by hand, and review the diff like any other change: an entry that disappears is a
fixed finding, an entry that appears is a finding someone chose to accept. Do not let a CI
job rewrite it on its own; that accepts every new finding.

```sh
sous-chef lint --write-baseline .sous-chef-baseline.json
git diff .sous-chef-baseline.json      # review, then commit
```

## GitHub Actions

The job checks out the layer repository and its dependencies, installs `sous-chef`, runs
lint with SARIF output and uploads it to code scanning.

**Getting the binary.** There is no release yet. Until there is, build it from the
repository with `cargo install --git` (a few minutes on a hosted runner; the runner's
preinstalled stable Rust is enough, sous-chef needs 1.89 or newer). Pin a commit with
`--rev` so a new commit on sous-chef's default branch cannot change your CI:

```sh
cargo install --locked --git https://github.com/yudjinn/sous-chef --rev <commit> sous-chef
```

Once a release exists, download its binary instead (README, "Install").

**Permissions.** Upload needs `security-events: write`; `contents: read` is what checkout
needs (and what code scanning requires in a private repository). A pull request from a
fork gets a read-only token and cannot upload; run the SARIF job on `push` and use
annotations on pull requests, or skip the upload step for forks. Code scanning is free on
public repositories; a private one needs GitHub Advanced Security (Code Security).

```yaml
name: lint layers
on:
  push:
    branches: [main]
  pull_request:

permissions:
  contents: read

jobs:
  sous-chef:
    runs-on: ubuntu-latest
    permissions:
      contents: read
      security-events: write
    steps:
      - uses: actions/checkout@v7.0.1
        with:
          submodules: recursive          # or a kas checkout step: `kas checkout kas/my.yaml`

      - name: Install sous-chef
        run: cargo install --locked --git https://github.com/yudjinn/sous-chef --rev "$SOUS_CHEF_REV" sous-chef
        env:
          SOUS_CHEF_REV: <commit>   # the full hash of a sous-chef commit you have checked

      - name: Lint (SARIF)
        id: lint
        # Exit 1 means new findings: keep going so they are uploaded, then fail below.
        # Exit 2 means lint could not run (no usable SARIF): the upload is skipped.
        run: |
          code=0
          sous-chef lint --format sarif --baseline .sous-chef-baseline.json > "$RUNNER_TEMP/sous-chef.sarif" || code=$?
          echo "code=$code" >> "$GITHUB_OUTPUT"

      - uses: github/codeql-action/upload-sarif@v4.39.3
        if: steps.lint.outputs.code == '0' || steps.lint.outputs.code == '1'
        with:
          sarif_file: ${{ runner.temp }}/sous-chef.sarif
          category: sous-chef

      - name: Fail on new findings
        if: steps.lint.outputs.code != '0'
        run: exit "$CODE"
        env:
          CODE: ${{ steps.lint.outputs.code }}
```

`category` names the analysis in the Security tab; give each SARIF upload of the same
repository its own (for example one per layer set). Drop `--baseline …` until you have
committed a baseline file, or the step exits 2.

Without code scanning (a private repository without Advanced Security, for example),
annotations show the findings on the pull request's diff instead (the run's summary page
shows them too; at most 10 of each type per step, see "Output formats"):

```yaml
      - name: Lint (annotations)
        run: sous-chef lint --format github --baseline .sous-chef-baseline.json
```

Each workflow example on this page passes [actionlint](https://github.com/rhysd/actionlint)
1.7.12 (with shellcheck 0.11.0).

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

## This repository's demo job

[`.github/workflows/lint-demo.yml`](../.github/workflows/lint-demo.yml) runs the path above
on sous-chef's own fixture workspace, `tests/fixtures/ws`, which has known findings (3
warnings and 1 info today). It runs on a push to `dev` and by hand (`gh workflow run
lint-demo.yml --ref <branch>`), not on pull requests. It builds sous-chef, then, from
`tests/fixtures/ws`:

1. `--format github --fail-on info`: the findings become annotations on the run; exit 1
   (findings) is expected and accepted, exit 2 fails the job;
2. `--format sarif`, checked by [`scripts/check-sarif.py`](../scripts/check-sarif.py)
   against the OASIS SARIF 2.1.0 schema (downloaded at a fixed commit, SHA-256 checked;
   not vendored, its licence is not MIT) and GitHub's requirements and limits, including
   that every result's file exists in the checkout;
3. the upload to code scanning under the category **`sous-chef-fixture`**;
4. `--write-baseline` then `--baseline --fail-on info`: zero new findings, exit 0.

The paths in the SARIF are relative to the repository root (`tests/fixtures/ws/meta-mine/…`),
because lint finds the git top level, so the alerts link to the fixture files.

**The fixture findings appear in the repository's Security tab** (Code scanning, tool
`sous-chef`, category `sous-chef-fixture`). They are test data, not problems in sous-chef. To get rid of them: dismiss
the alerts (`Dismiss alert` → "Used in tests"), or delete the analyses, which removes
their alerts, and stop the upload (delete the upload step or the workflow) so they do not
come back:

```sh
# The analyses of the demo, newest first; an analysis can be deleted when "deletable" is true.
gh api "repos/{owner}/{repo}/code-scanning/analyses?tool_name=sous-chef" \
  --jq '.[] | select(.category == "sous-chef-fixture") | [.id, .ref, .created_at, .deletable] | @tsv'
# Delete one; confirm_delete also allows deleting the last analysis of the category.
gh api -X DELETE "repos/{owner}/{repo}/code-scanning/analyses/<id>?confirm_delete=true"
```

The DELETE answer gives `next_analysis_url` and `confirm_delete_url` for the next one of the
same set; repeat until none is left.

## What CI does not check

CI runs the test suite, which replays recorded BitBake output (`tests/fixtures/outputs/`,
including the recorded live runs under `outputs/live/`), but not **live accuracy**: whether
sous-chef still agrees with `bitbake -e` and real builds on a real Yocto workspace. Those
tests are `#[ignore]`d and need a multi-gigabyte checkout and a build host; a maintainer
runs them with `scripts/differential.sh` and the steps in
[live-verification.md](live-verification.md) (environment: [live-environment.md](live-environment.md)).

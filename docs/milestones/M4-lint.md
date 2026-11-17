# M4: Lint

Checks layers against Yocto conventions: override syntax and operators, organisation of
recipes / `.inc` / classes / layers, and recipe hygiene.

Read first: [lint-rules.md](../lint-rules.md), [bitbake-semantics.md](../bitbake-semantics.md).

Parallelism: **T4.1** first (wave 3; it needs only the parser and the index query layer).
Then **T4.2**, **T4.3**, **T4.4** go to three agents: each rule is its own file. **T4.5**
needs T4.1 and T2.5.

Rules for rule authors:

- A rule file contains the rule and nothing shared. Shared helpers go in
  `src/lint/helpers.rs`, owned by T4.1; ask for additions in your hand-off instead of editing
  it concurrently, or add a private helper in your own file.
- Every rule has `tests/fixtures/lint/<rule-id>/bad.*` and `good.*`, and a test asserting the
  exact findings (rule id, line) on `bad` and none on `good`.
- False positives are worse than misses. When a rule cannot be sure, lower the severity or do
  not report.
- Messages are one line: what is wrong, then what to write instead.

---

## T4.1 Engine, configuration, CLI

**Depends on:** T1.2, T2.3 (T3.4 optional, see below).
**Owns:** `src/lint/mod.rs`, `src/lint/helpers.rs`, the `lint` branch in `src/main.rs`

- `LintContext` construction for each scope: `File` (parsed file + its `Loc` + index),
  `Recipe` (adds the `DataStore` from `evaluate`; if M3 is not merged yet, recipe-scope rules
  are skipped with a note), `Layer` (index + workspace).
- `run(target, options) -> Vec<Finding>`: resolve the target (file pointer, recipe, layer,
  or whole workspace), pick rules by scope, apply `disable` and severity overrides from
  config, sort findings by file then line, de-duplicate.
- Inline suppression: a comment `# sous-chef: allow <rule-id>` on the line before a statement.
- Upstream layers (not writable) are skipped by default when linting the whole workspace;
  `--all` includes them.
- CLI: `sous-chef lint [TARGET] [--rule ID]... [--severity LEVEL] [--format text|json]`,
  exit code 1 if any finding at or above `--fail-on` (default `error`).
- Helpers: classify a variable as list-like or not (a table of well-known variables plus a
  heuristic), package-scoped variable list, known-override set from the index.

**Done when:** tests for target resolution, disable by id and by group, inline suppression,
sorting, exit codes; `hygiene/parse-error` (trivial, implemented here as the sample rule)
passes its fixture test.

## T4.2 Rules: group `override`

**Depends on:** T4.1. **Owns:** `src/lint/rules/override_*.rs`,
`tests/fixtures/lint/override/**`

Implement the eleven rules in the `override` group of lint-rules.md.

Notes:
- `legacy-syntax`: use `VarRef.legacy_syntax` from the parser; the message gives the colon
  form to write.
- `append-no-space`: only for list-like variables; skip values starting with `${@`.
- `dead` and `shadowed` are recipe scope and need M3; implement them against the `DataStore`
  API and gate their tests on it.
- `plus-equals-on-default`: needs an index query for `?=` assignments of the same base in the
  workspace.

**Done when:** every rule has passing bad/good fixture tests, and running the group over the
whole fixture workspace (excluding `lint/`) yields only findings listed in an expected file.

## T4.3 Rules: group `org`

**Depends on:** T4.1. **Owns:** `src/lint/rules/org_*.rs`, `tests/fixtures/lint/org/**`

Implement the seventeen rules in the `org` group.

Notes:
- `duplicate-versions` / `repeated-logic`: compare normalised statement lists (ignore
  whitespace, comments, version-data variables). Thresholds: at least 60 percent of
  statements identical for versions; identical function body of at least 5 lines in 3 or more
  recipes for repeated logic. Report once, on the newest file, listing the others as pointers.
- `class-export-functions`: a class `foo.bbclass` defining `foo_do_compile` should have
  `EXPORT_FUNCTIONS do_compile`.
- `append-no-match` uses `orphan_appends()` from the query layer.
- `upstream-edit` shells out to `git status --porcelain` in the layer root with a timeout;
  silently skips if not a git checkout or git is missing.
- `layer-conf` is several checks under one id; put the specific problem in the message.

**Done when:** as T4.2. For layer-scope rules the fixtures are small layers under
`tests/fixtures/lint/org/<rule>/bad-layer` and `good-layer`.

## T4.4 Rules: group `hygiene`

**Depends on:** T4.1. **Owns:** `src/lint/rules/hygiene_*.rs`,
`tests/fixtures/lint/hygiene/**`

Implement the remaining eight rules (`parse-error` is done in T4.1).

Notes:
- `missing-metadata` and `src-uri-checksum` are recipe scope (values may come from an `.inc`
  or class); without M3 fall back to a search of the static tree.
- `host-path`: scan shell function bodies; ignore comments, `${...}` paths and here-docs that
  write config files under `${D}`.
- `variable-order` is disabled by default.
- `workdir-s`: confirm which release rejects `S = "${WORKDIR}"` and only report for layers
  whose compat includes it.

**Done when:** as T4.2.

## T4.5 Tool: `lint`

**Depends on:** T4.1, T2.4, T2.5. **Owns:** `src/mcp/tools/lint.rs`

Per mcp-tools.md. Thin wrapper over `lint::run` with table rendering and budget. A summary
line first: counts by severity and by rule, so a truncated table still tells the whole story.
Also serve the rule catalogue as the `sous-chef://rules` resource content.

**Done when:** snapshot test on a fixture layer; budget truncation keeps the summary line.

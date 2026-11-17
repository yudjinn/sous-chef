# M1: Parser and workspace

Turns files on disk into statements with exact spans, and a build directory into a list of
layers with a working file search.

Read first: [bitbake-semantics.md](../bitbake-semantics.md), the parser and workspace
contracts in [architecture.md](../architecture.md).

Parallelism: **T1.1 -> T1.2** is one stream. **T1.3** and **T1.5** are a second stream and
can start at the same time. **T1.4** needs T1.2 and T1.3.

---

## T1.1 Lexer

**Depends on:** M0. **Owns:** `src/parse/lexer.rs`

Split source into *logical lines*, each with its `LineSpan` and byte range:

- Join lines ending in `\` (record the physical span). Trailing whitespace after `\` does not
  continue; emit a diagnostic.
- Classify each logical line cheaply: blank, comment, function header (`name() {`,
  `python name() {`, `fakeroot ...`), `def` header, directive keyword, or other.
- Collect function bodies: from the header to the first line that is exactly `}` at column 0.
  Collect `def` bodies: following lines that are indented or blank, stopping before the
  first non-blank line at column 0.
- Handle CRLF, a missing final newline, a UTF-8 BOM, and non-UTF-8 bytes (lossy decode; never
  fail).
- An unterminated function body ends at end of file with a diagnostic.

**Done when:** tests over `tests/fixtures/syntax/` assert the logical line spans for
continuation, bodies, `def` blocks, CRLF input and an unterminated body.

## T1.2 Parser

**Depends on:** T1.1. **Owns:** `src/parse/parser.rs`, `src/parse/mod.rs`

Produce `ParsedFile` per the contract.

- Assignments: all eight operators, single or double quotes, `export` prefix, flags
  (`VAR[flag]`), names containing `${...}`, `:`, `-`, `+`, `.`, `/`.
- `VarRef` splitting: split the name on `:` outside `${...}`; first element is `base`. For
  legacy detection apply the rules in bitbake-semantics.md and set `legacy_syntax` (then
  `base`/`overrides` reflect the legacy split).
- Functions: shell, `python`, `fakeroot`, `fakeroot python`, anonymous (`python () {`,
  `python __anonymous () {`), override syntax in names (`do_install:append:class-target`).
- Directives: `inherit`, `inherit_defer`, `require`, `include`, `include_all`, `addtask`
  (with any order and repetition of `before` / `after`), `deltask`, `addhandler`,
  `EXPORT_FUNCTIONS`, `export VAR`, `unset VAR`, `unset VAR[flag]`.
- Anything unrecognised becomes `Unknown` with a diagnostic. The parser never returns an
  error and never panics.

Before finishing, confirm the items marked *verify* in bitbake-semantics.md that concern
syntax (`inherit_defer`, `include_all`, `addtask` forms) and correct the doc.

**Done when:**
- Snapshot tests (`insta`) for every file in `tests/fixtures/syntax/`.
- Every statement form in bitbake-semantics.md has at least one test.
- A property-style test feeds truncated and byte-shuffled fixture files and asserts no panic
  and that statement spans are ordered and non-overlapping.
- Legacy detection: tests for true positives and for the false-positive list in the
  semantics doc.

## T1.3 Layer discovery

**Depends on:** M0. **Owns:** `src/workspace/mod.rs`, `src/workspace/layers.rs`

- Find the build directory: `Config.build_dir`, else walk up from the working directory for
  `conf/bblayers.conf`.
- Read `BBLAYERS` from `bblayers.conf`. Until T1.2 is merged, use a minimal private
  assignment reader; switch to `parse()` afterwards. Expand the handful of variables that
  appear there (`${TOPDIR}`, `${OEROOT}`, `${BSPDIR}`-style variables assigned in the same
  file, `${@os.path.abspath(...)}` is reported as unresolved, not guessed).
- For each layer read `conf/layer.conf`: collections, priority, pattern, compat, depends,
  `BBFILES` globs with `${LAYERDIR}` substituted.
- No build directory: use `Config.layers`, else every directory under the working directory
  that contains `conf/layer.conf` (depth limit 4).
- Layer `name` is the directory name; if two layers share one, disambiguate with the parent
  directory (`poky/meta`, `other/meta`).
- Implement `Workspace::loc_for` and `abs_path`, and the confinement check
  (`Workspace::confine(path) -> Result<PathBuf>`, resolving symlinks).

**Done when:** tests on the fixture workspace assert layer order, priorities, collections,
`BBFILES`, the no-build-dir fallback, name disambiguation, and that `confine` rejects `..`
escapes and symlinks pointing outside.

## T1.4 Path resolution

**Depends on:** T1.2, T1.3. **Owns:** `src/workspace/resolve.rs`

- `find_in_bbpath`: resolve `require` / `include` targets (absolute, relative to the
  including file, then each `BBPATH` entry). Confirm the search order (*verify* item).
- `find_class`: `classes-recipe/` then `classes/` for recipe inherits; `classes-global/` then
  `classes/` for global ones; return all candidates in priority order through a second
  function so `class` can show shadowing.
- `append_matches`: exact name match and `%` wildcard semantics.
- `recipe_files(ws)`: expand each layer's `BBFILES` globs into recipe and append paths.
- Paths containing `${VAR}`: expose `resolve_with(vars: &dyn Fn(&str) -> Option<String>)`
  so the scanner can resolve the common static cases (`${BPN}`, `${PN}`, `${PV}`, `${BP}`,
  `${THISDIR}`, `${LAYERDIR}`, `${COREBASE}`) and the evaluator can resolve the rest.

**Done when:** fixture tests cover: include found next to the file, found via `BBPATH`, not
found; class in each of the three directories and shadowed across layers; `hello_%.bbappend`
and `hello_1.1.bbappend` matching, `orphan_9.%.bbappend` matching nothing.

## T1.5 Configuration

**Depends on:** M0. **Owns:** `src/config.rs`

- Parse `sous-chef.toml` per architecture.md; merge with CLI flags (flags win).
- Validate: paths exist, `writable_layers` name real layers (checked later against the
  workspace, so provide `Config::validate(&Workspace)`).
- Sensible defaults when no file exists.

**Done when:** tests cover missing file, full file, flag override, and each validation error.

## T1.6 Release detection

Added 2026-11-17, after wave 1. Done by the orchestrator during wave 2 integration, because
it changes shared contracts and wave 3 (lint engine, jobs) builds on it.

**Depends on:** T1.3, T1.5. **Owns:** `src/release.rs` (new), and small declared edits to
`src/config.rs` (`release`, `[lint.severity]`), `src/workspace/` (`Workspace.release`,
`Layer::lint_releases`) and `src/mcp/tools/status.rs` (the `release:` line).

- `Release`, `Feature`, `ReleaseInfo` and detection as in "Release contract" in
  architecture.md.
- The "Releases" table in bitbake-semantics.md: one row per `Feature`, with the first and
  last release that has it and the source checked (migration guide or source). A row that
  could not be checked is marked *verify* and its `Feature` is not used to produce errors.
- The fixture workspace declares Wrynose (`LAYERSERIES_CORENAMES` in its core layer).

**Done when:** tests for each detection source and their order, an unknown newer codename,
a codename older than the floor, `lint_releases` for a writable layer claiming
`kirkstone scarthgap` in a Wrynose workspace and for a non-writable layer, and `has()` for
every `Feature` at Kirkstone, Scarthgap and Wrynose.

# M8: Verification and extras

Proves the model is right against real BitBake, checks performance at real scale, and adds
the extra views that build on the finished core.

Parallelism: all tasks are independent of each other. T8.1 and T8.2 need a machine with a
poky checkout and are opt-in; they must not run in default `cargo test`.

---

## T8.1 Differential test against BitBake

**Depends on:** M3, T5.5. **Owns:** `tests/differential.rs`, `xtask/` or
`scripts/differential.sh`

The main check that the override-chain model is correct.

- Opt-in via `SOUS_CHEF_POKY=<path to an initialised build dir>`; skipped otherwise.
- For a sample of recipes (start with about 30 covering: plain autotools, cmake, a kernel, a
  native recipe, an image, recipes with many bbappends in meta-openembedded, recipes using
  `PACKAGECONFIG`, `BBCLASSEXTEND`), run `bitbake -e` and compare, for every variable:
  the final value, and the list of files in the history.
- Classify each mismatch:
  - **explained**: the static value is marked `unevaluated_python` or `unresolved`;
  - **bug**: anything else.
- Output a report (counts per class, top variables by mismatch) and fail on any **bug** for a
  tracked list of important variables (`SRC_URI`, `DEPENDS`, `RDEPENDS:*`, `FILES:*`,
  `PACKAGECONFIG`, `EXTRA_OECONF`, `EXTRA_OECMAKE`, `S`, `B`, `PV`, `LICENSE`, `OVERRIDES`,
  `FILESPATH`, `PREFERRED_PROVIDER_*`).
- Fix evaluator bugs found, or file each as a documented limitation in
  bitbake-semantics.md ("What the static evaluator cannot do").
- Resolve every remaining *verify* marker in bitbake-semantics.md.

**Done when:** the report runs on poky + meta-openembedded; zero **bug** mismatches on the
tracked variables for the sample; no *verify* markers remain.

## T8.2 Scale and performance

**Depends on:** M2, M3. **Owns:** `benches/`, `docs/performance.md`

- Measure on poky + meta-openembedded (and one BSP layer): full index time, index size,
  no-change refresh time, single-file refresh, `recipe` / `search` / `var` latency cold and
  warm, configuration-pass time, memory.
- Targets: no-change refresh under 150 ms; `var` warm under 100 ms; full index under 30 s.
- If the no-change refresh misses the target, implement the fallback in this order and
  re-measure: (1) skip unchanged directories by directory mtime where safe, (2) refresh at
  most once per N seconds per process, (3) an inotify watcher.
- Record results and the machine in `docs/performance.md`.

**Done when:** the numbers are recorded and the targets are met or the gap is explained.

## T8.3 Extra views

**Depends on:** M3. **Owns:** new `show` modes in existing tools as listed; coordinate with
the owner of each tool file if it is still open.

Add, each with snapshot tests:

- **`recipe show=sources`** (in `recipe.rs`): each `SRC_URI` entry, its type, the file on
  disk a `file://` entry resolves to along `FILESPATH` (with the search order when not
  found), the patch status header (`Upstream-Status`) of each patch, and which layer
  contributed the entry.
- **`recipe show=tasks`** (in `recipe.rs`): the task graph in order, from `addtask` /
  `deltask` across the recipe and its classes, each with the pointer(s) of the pieces that
  compose it (`do_install`, `do_install:append` from a bbappend, the class default).
- **`recipe show=packages`** (in `recipe.rs`): `PACKAGES`, and per package `FILES`,
  `RDEPENDS`, `RPROVIDES`, with pointers.
- **`layers show=compat`** (in `layers.rs`): compat series mismatch between layers, missing
  `LAYERDEPENDS`, dependency order problems in `BBLAYERS`.

## T8.4 Release diff (optional)

**Depends on:** M2. **Owns:** `src/mcp/tools/diff.rs` (new deferred tool `diff`; add to the
registry and to mcp-tools.md)

Statement-level diff of a recipe tree between two git revisions of a layer: added, removed
and changed variables and tasks, version renames (`foo_1.0.bb` to `foo_1.1.bb` treated as
the same recipe), and which of your bbappends are affected (appends that no longer match,
patches that touch changed files). Reads old revisions with `git show` into the parser; no
checkout.

**Done when:** tests on a small fixture git repository built in a temp dir.

## T8.6 Workspace audit report (not scheduled)

Added 2026-11-17 at the user's request, to be scheduled after wave 5.

**Depends on:** M4 (all rule groups), T1.6, T2.3, T8.3 (`layers show=compat`).
**Owns:** `src/audit.rs` (new), an `audit` subcommand in `src/main.rs`, and
`src/mcp/tools/audit.rs` (new deferred tool `audit`; add to the registry and to
mcp-tools.md)

A one-off analysis of a whole build repository, the kind that holds its layers as git
submodules, producing one large findings report for a person to read. It adds no analysis
of its own: it runs what the other milestones built over everything and assembles the
result.

- Input: a repository root. Find every layer under it (`conf/layer.conf`, following
  `.gitmodules`), whether or not `bblayers.conf` enables it; a layer that is present but
  not enabled is listed as such.
- Report sections:
  1. **Inventory**: per layer, its submodule path, remote and revision, whether the checkout
     is dirty, collection, priority, `LAYERSERIES_COMPAT`, writable or upstream, and counts
     of recipes, appends, classes and conf files.
  2. **Releases**: the detected release and its source, and a matrix of layer against
     claimed release, marking layers that do not claim the workspace's release.
  3. **Layer structure**: `LAYERDEPENDS` gaps and `BBLAYERS` order problems
     (`layers show=compat`), shadowed recipes and classes, orphan and over-broad bbappends,
     duplicate versions.
  4. **Lint findings**: every rule over every layer (as `lint --all`), grouped by layer and
     rule with counts, the layer's own code separated from upstream layers, each finding
     with its pointer. Release-dependent findings name the release.
  5. **Parse and resolution problems**: unparsed statements, unresolved `include` /
     `require` / `inherit` targets.
  6. **What was not checked**: values that depend on unevaluated python, and anything that
     needs BitBake when no build environment was found.
- Output: `sous-chef audit [ROOT] [--format md|json] [--output FILE]` writes the whole
  report; it is not subject to the token budget. The MCP tool stores the report under a
  handle and returns a digest (counts per section and the worst findings) with the handle,
  like `log`.
- Read-only: never writes into the repository, never runs a build.

**Done when:** a snapshot test of the report on the fixture workspace in both formats; a
temp-dir test with a layer present on disk but absent from `bblayers.conf`, and with a
`.gitmodules` file; the MCP tool's digest stays under the budget and its handle returns the
full report through `slice`.

## T8.5 Documentation pass

**Depends on:** everything else. **Owns:** `README.md`, `docs/mcp-tools.md`,
`docs/lint-rules.md`, `sous-chef://help` content

- Bring the docs in line with what was built: arguments, defaults, output samples taken from
  real snapshot tests.
- A "limits" section in the README: what is static, what needs BitBake, what is never
  evaluated.
- A short troubleshooting section (index location, how to rebuild, hook debug log).

**Done when:** every tool and rule in the code is in the docs and the reverse; the samples in
the docs match current output.

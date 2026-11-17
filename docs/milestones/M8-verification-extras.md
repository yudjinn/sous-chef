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

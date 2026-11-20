# Changelog

All notable changes to sous-chef are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0, a minor version may
change tool arguments, output formats and configuration keys.

## [Unreleased]

## [0.1.0] - 2026-11-20

The first release: one Rust binary that serves MCP over stdio for Yocto / OpenEmbedded
layers, a CLI, and a Claude Code plugin. Answers are **pointers** into the layers
(`layer:path:A-B` plus a one-line snippet) instead of whole files, and recipe files are
replayed the way BitBake does: includes, inherits, bbappends in layer order, then the
override chain.

### Added

- **MCP server** (`sous-chef serve`) with 18 tools:
  - `recipe`: a recipe's files, versions (and which one BitBake picks), bbappends in
    application order, classes and key values; its include/inherit tree, functions,
    sources and patches, tasks, packages and CVE view.
  - `item`, `search`, `class`: the exact text of a function or assignment (usable as an
    Edit string); statements by name, literal text, regex or full text, each with its
    enclosing item; which class file wins and who inherits it.
  - `var`, `overrides`: the final value of a variable for a recipe, machine and distro with
    every operation in parse order and where it comes from; with `authoritative: true`,
    BitBake's own value (`bitbake -e`, cached). The evaluated `OVERRIDES` chain and
    overrides that match nothing.
  - `deps`, `layers`: build, runtime, `PACKAGECONFIG` and cross-configuration
    dependencies, reverse dependencies and providers; layers, priorities, compatibility,
    shadowed recipes and bbappend matches.
  - `lint`, `scaffold`: lint findings and fixes (below); new recipes, bbappends, layers,
    classes and `.inc` splits as a dry run or written into a writable layer, and where a
    change belongs.
  - `bitbake`, `log`, `slice`, `sigs`, `pkgdata`: `bitbake`, `devtool` and
    `bitbake-layers` as background jobs with progress, cancellation and timeouts; a failed
    task's log digested; large output kept under a handle and read in parts; why a task
    re-ran (a `bitbake-diffsigs` digest); which package ships a file, from a build's package
    data.
  - `status`, `audit`, `diff`: the workspace, release, index and build environment; one
    findings report over every layer of a build repository; a statement-level diff of a
    layer between two git revisions, and which of your bbappends no longer apply.
- **Static evaluator.** Inline python made only of an allowlist of pure helpers
  (`bb.utils.contains`, `oe.utils.conditional`, `d.getVar`, string methods, some
  OE-core class functions, ...) is evaluated with python's semantics; anything else, and
  every anonymous python function, is never run, and a value that depends on it is
  flagged instead of guessed. `BBCLASSEXTEND` variants, `BBMULTICONFIG` configurations
  (`mc=<name>` or `mc:<name>:<recipe>`), a static `BBMASK` and BitBake's skip conditions
  ("would skip" / "may skip") are modelled. `[build_host] arch` / `os` in `sous-chef.toml`
  give the build host's `os.uname()[4]` and `os.uname()[0].lower()` (`BUILD_ARCH`,
  `BUILD_OS`), so native and nativesdk recipes evaluate exactly; unset, those values are
  flagged (sous-chef never reads its own host's).
- **Lint** (`lint` tool and `sous-chef lint`): 42 rules in three families (`hygiene/`,
  `org/`, `override/`), each layer checked against every release its `LAYERSERIES_COMPAT`
  claims; `--fix` prints safe fixes as a diff and `--write` applies them through the
  scaffold writer (`--fix-suggested` for meaning-changing ones); `--as-release` and
  `--migrate-to` for a release the layer does not claim yet; `--format sarif` (SARIF 2.1.0)
  and `--format github` (workflow annotations) for CI; `--baseline` /
  `--write-baseline` for accepted findings. Exit status 0 clean, 1 findings at or above
  `--fail-on`, 2 lint could not run.
- **CLI**: `serve`, `index`, `lint`, `audit` (Markdown or JSON report of a whole build
  repository), `diff`, `hook`; global `--build-dir`, `--config`, `--layer`.
- **Claude Code plugin** (`plugin/`): the MCP server, a skill (`sous-chef:yocto-layers`)
  and `PostToolUse` hooks, all failing open (no output on any error, a missing index or
  more than 150 ms):
  - Grep and Read hits in Yocto files get their enclosing item and, for a recipe, the
    files that also contribute; a plain `grep -n` / `rg -n` through Bash gets the same.
  - The output of a large `bitbake`, `devtool` or `bitbake-layers` command run through
    Bash (also through `kas build` / `kas shell -c`) is replaced by a digest, the full text
    kept under a handle.
  - An edited file is reindexed and linted; an edit in a layer that is not writable gets a
    pointer to the bbappend to write instead.
  - Each hook can be switched off under `[hooks]` in `sous-chef.toml`.
  - The repository is a plugin marketplace: `claude plugin marketplace add
    yudjinn/sous-chef#v0.1.0`, then `claude plugin install sous-chef@sous-chef`. The
    plugin runs the `sous-chef` binary from `PATH`; install it first.
- **Build environments**: OE's `oe-init-build-env`, bitbake-setup's `init-build-env`, the
  environment the server was started in, and **kas** projects. kas files are read as kas
  merges them (includes, lock files) without running kas; `KAS_MACHINE`, `KAS_DISTRO`,
  `KAS_TARGET` and `KAS_TASK` apply as in kas; BitBake runs through `kas shell`.
  Every BitBake command is started without the host's `TMPDIR`;
  `[bitbake] pass_env = ["TMPDIR"]` passes it after all.
- **Releases**: Wrynose (6.0 LTS) is the primary target, Scarthgap (5.0 LTS) is fully
  supported, Kirkstone (4.0 LTS) is the floor (best effort, no fixture workspace). Older
  releases are out of scope.
- **Release binaries** for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (glibc
  2.35 or newer), `x86_64-apple-darwin` and `aarch64-apple-darwin`, with `SHA256SUMS`.

### Accuracy

Static values compared with `bitbake -e` on the same workspace, one machine and distro per
release, latest run 2026-11-20 ([docs/differential.md](https://github.com/yudjinn/sous-chef/blob/v0.1.0/docs/differential.md), "After
build-host"; the first three rows with `[build_host]` set to the build host's values):

| Release | Values compared | Equal to BitBake (+ whitespace only) | Known gap | Differ without a flag |
|---|---|---|---|---|
| Wrynose (6.0), yocto-kiss, 23 recipes and 2 variants | 1090 | 1022 (+1) | 58 | 0 |
| Scarthgap (5.0), OE-core, meta-yocto, meta-oe, 28 targets | 1186 | 1156 (+3) | 5 | 0 |
| Kirkstone (4.0), same layers, 28 targets | 1145 | 1094 (+3) | 1 | 0 |
| Scarthgap, `mc:musl` and default configuration, 6 targets | 659 | 600 | 13 | 0 |

Every other value that differs is flagged, with its cause (`note: uncertain: ...`); for
those, `var` with `authoritative: true` asks BitBake. Functions and variable flags were not
compared.

Lint precision, measured on a stratified random sample of 150 `lint --all` findings over the
three releases ([docs/lint-rules.md](https://github.com/yudjinn/sous-chef/blob/v0.1.0/docs/lint-rules.md), "Measured precision"): 15 false
positives found, 13 fixed before this release. `override/remove-in-recipe` is info by
default because every sampled finding of it was correct but unhelpful on upstream layers;
`"override/remove-in-recipe" = "warning"` under `[lint.severity]` makes it a warning.

[Unreleased]: https://github.com/yudjinn/sous-chef/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/yudjinn/sous-chef/releases/tag/v0.1.0

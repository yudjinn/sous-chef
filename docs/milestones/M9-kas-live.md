# M9: kas support and live verification

Added 2026-11-17 at the user's request, after wave 4. Two things: sous-chef should understand
projects set up with [kas](https://kas.readthedocs.io), and the behaviour that earlier
milestones could only check against source code should be checked against a real, working
build environment.

Read first: [bitbake-semantics.md](../bitbake-semantics.md) (every item marked *verify*),
the "Open items" lists in [orchestration.md](../orchestration.md), the kas documentation
(project configuration format, `kas checkout`, `kas shell`, `kas dump`, lock files). Do not
write kas behaviour from memory: check the kas documentation or source and record what was
confirmed, with the kas version, in bitbake-semantics.md.

Parallelism: **T9.1** (live environment) first; **T9.2** needs it. **T9.3** (kas support)
is independent of T9.2 and can run beside it, using the environment from T9.1 for its tests.

## The reference environment

[bootlin/yocto-kiss](https://github.com/bootlin/yocto-kiss), branch `wrynose`: a small,
realistic, working setup. Read from its README on 2026-11-17:

- Yocto Wrynose. Its own layers `meta-kiss`, `meta-kiss-ti`, `meta-kiss-st`,
  `meta-kiss-nxp`, `meta-kiss-amd` are in the repository; kas downloads `bitbake`,
  `openembedded-core`, `meta-arm`, `meta-openembedded` (`meta-oe`, `meta-python`) and
  `meta-xilinx` (`meta-microblaze`).
- kas files: `kas/kiss.yaml` plus one of `kas/dogbonedark.yaml`, `kas/stompduck.yaml`,
  `kas/freiheit93.yaml`, `kas/krazymp.yaml` (for example
  `kas build kas/kiss.yaml:kas/dogbonedark.yaml`). kas generates `build/conf/bblayers.conf`.
- Distro `kiss`, image `kiss-image`, `TMPDIR` is `build/tmp-glibc`.
- A `bitbake-setup` alternative: `yocto-kiss.conf.json`,
  `bitbake-setup init <url>`, then `. bitbake-builds/kiss-wrynose/build/init-build-env`.
  This is the Wrynose environment setup that `BbEnv` does not know yet.

## Limits on live runs (binding)

The user's rule: no large builds. For every task in this milestone:

- **Allowed without asking:** cloning the repository and its layers (`kas checkout`);
  parse-only commands (`bitbake -p`, `bitbake -e [<recipe>]`, `bitbake-getvar`,
  `bitbake-layers show-*`, `kas dump`, `kas shell -c` running one of these); `-c fetch`,
  `-c unpack` and `-c patch` of one small recipe.
- **Not allowed without asking the user first:** anything that compiles. Building even one
  small target recipe such as `curl` from an empty sstate cache first builds the native
  tools and the cross toolchain, which takes a long time and tens of gigabytes. If a task
  needs a real task failure (for a build log sample), produce it without compiling: a
  `do_fetch` of a deliberately wrong URL, or a `do_patch` of a deliberately broken patch,
  in a scratch recipe in a scratch layer.
- **Never:** an image build (`kiss-image` or any other), `kas build` without `-c`/target
  restrictions, `cleanall` on shared directories.
- The environment lives outside the repository, in a directory the user names or under the
  job's scratch space; `SOUS_CHEF_LIVE` points at it. No test in the normal gate may need
  it: live tests are `#[ignore]` unless `SOUS_CHEF_LIVE` is set.
- Downloads and sstate stay inside that directory. Nothing is pushed anywhere.

---

## T9.1 Live environment

**Depends on:** nothing. **Owns:** `xtask/` or `scripts/live-env.sh` (new),
`docs/live-environment.md` (new)

- A script that sets the environment up reproducibly: installs kas into a private virtual
  environment (not system-wide), clones `yocto-kiss` at a pinned commit, runs
  `kas checkout kas/kiss.yaml:kas/dogbonedark.yaml`, and prints the paths. It records the
  kas version, the yocto-kiss commit and each layer's commit.
- Confirm that the parse-only commands work from the result (`bitbake -p` completes) and
  record how long each takes and how much disk the checkout uses.
- Also set up the `bitbake-setup` variant if that is cheap (it shares the layers'
  download), since it is the Wrynose form `BbEnv` must learn.

**Done when:** the script runs from scratch on the user's machine; `docs/live-environment.md`
says how to create, find and delete the environment and what it costs; `bitbake -e curl`
produces output.

## T9.2 Verify against the live environment

**Depends on:** T9.1. **Owns:** `tests/live_*.rs` (new, `#[ignore]` without
`SOUS_CHEF_LIVE`), recorded samples under `tests/fixtures/outputs/live/`, and corrections to
`docs/bitbake-semantics.md`. Code fixes for what it finds are separate tasks, listed in the
hand-off, unless they are a few lines.

Run sous-chef against the environment and settle, with evidence, the items earlier
milestones left unverified:

- **A real `bitbake -e` sample** (trimmed) for T5.5 and for `slice var=`: confirm the
  history comment format, `export` / `unset` lines, functions.
- **Static evaluation against BitBake** (the T8.1 comparison, on a handful of recipes
  including `curl`): final values of `SRC_URI`, `DEPENDS`, `PACKAGECONFIG`, `OVERRIDES`,
  `FILE_LAYERNAME`, `S`, `WORKDIR`; classify each difference (python not evaluated, a
  known gap, or a bug).
- **Python in `OVERRIDES`**: how much of the real override list the static evaluator
  knows, and what `override/dead` reports on the `meta-kiss*` layers.
- **bbappend order** against `bitbake-layers show-appends`; **layer priorities** against
  `show-layers`; **shadowed recipes** against `show-overlayed`.
- **Provider selection** (`PREFERRED_PROVIDER`, `PREFERRED_VERSION`) against
  `bitbake-getvar` / `bitbake -e`, including a `-native` (`BBCLASSEXTEND`) case.
- **The environment runner**: `BbEnv::detect` on a kas checkout and on a `bitbake-setup`
  build (`init-build-env`); whether the BitBake server leaves the process group on a
  cancel; whether a second client waits or fails on the lock.
- **Logs**: the `T` directory layout under `tmp-glibc`, `bitbake-getvar --value`, a real
  failed-task log (produced without compiling, see the limits) through `log` and the Bash
  hook's digest.
- **Lint on real layers**: run the default rule set on the `meta-kiss*` layers and, with
  `--all`, on OE-core. List every finding that is a false positive.
- **Scale**: index build time and size, a no-change refresh, hook latency, on this
  workspace (the T8.2 numbers, for one real workspace).

**Done when:** each item above is marked confirmed, corrected (with the doc fixed) or still
open with the reason, in the hand-off and in bitbake-semantics.md; the recorded samples are
committed; a list of bugs found, each with a minimal reproduction.

## T9.3 kas project support

**Depends on:** T1.3, T5.3 (merged). **Owns:** `src/workspace/kas.rs` (new),
`src/bitbake/env.rs` (a new mode), `src/lint/rules/org_generated_conf_edit.rs` (new rule),
and declared edits in `src/workspace/mod.rs`, `src/config.rs`, `src/mcp/tools/status.rs`,
`src/hook/bash.rs`, `src/scaffold/layer.rs`, the rule registry and `docs/lint-rules.md`

- **Detection.** A project is a kas project when `sous-chef.toml` names kas files
  (`kas = ["kas/kiss.yaml", "kas/dogbonedark.yaml"]`) or, without that, when the
  environment says so (`KAS_BUILD_DIR`, a `.config.yaml` kas wrote) or kas files are found
  next to the build directory. `status` shows the kas files in use.
- **Reading the project.** Parse the kas configuration (with `includes`, and the
  `file1:file2` combination): repos and their layers, refspecs or commits, `machine`,
  `distro`, `target`, `local_conf_header`, `bblayers_conf_header`. Use it for: default
  `MACHINE` / `DISTRO` and target when no build directory exists yet; the layer list before
  `kas checkout` has run (layers not yet cloned are reported, not an error); which layers
  are the project's own (in the repository) and so writable by default, and which are
  downloaded upstream layers.
- **Running BitBake.** A `BbEnv` mode that runs commands through
  `kas shell <files> -c '<command>'`, with the same argv safety as the other modes (the
  command string kas runs is built from validated names only). Also a mode for a
  `bitbake-setup` build (`init-build-env`).
- **Lint.** A new rule `org/generated-conf-edit` (W, file): `build/conf/local.conf` and
  `bblayers.conf` are generated by kas and overwritten on the next run; a setting that
  appears there and not in a kas file's `local_conf_header` will be lost. Report it and
  say which kas file to put it in. The existing `org/recipe-change-in-local-conf` points at
  the kas file in a kas project.
- **Hooks.** The Bash hook recognises `kas build ...` and `kas shell ... -c "bitbake ..."`
  as BitBake commands (a known miss today).
- **Scaffold.** In a kas project, `scaffold kind=layer` adds the new layer to the kas file
  (a `Modify` of the YAML, preserving comments and order) instead of `bblayers.conf`, and
  says so.
- **Later, not in this task:** the audit report (T8.6) lists each repo's pinned commit and
  whether a lock file exists; a `kas` section in the plugin's skill text (T7.5).

**Done when:** tests on a small kas project fixture (two repos, an include, a machine
file): discovery before and after checkout, defaults from the YAML, own and upstream
layers, the `kas shell` argv (with the fake kas script), the new rule on a fixture, the
Bash hook on a recorded `kas build` event, the scaffold `Modify`; and, with
`SOUS_CHEF_LIVE`, discovery of the yocto-kiss project matching `bitbake-layers show-layers`.

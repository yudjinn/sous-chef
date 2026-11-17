# M6: Scaffold

Creates Yocto files in the right place with the right conventions. Everything is a dry run
unless `write: true`.

Read first: `scaffold` in [mcp-tools.md](../mcp-tools.md), the `org` rules in
[lint-rules.md](../lint-rules.md) (generated output must pass them).

Releases: generated files follow the workspace release (`ws.release`, see "Release contract"
in architecture.md): the `LAYERSERIES_COMPAT` value and the `S` / `UNPACKDIR` conventions.

Parallelism: **T6.1** first (wave 2, needs the workspace and config only). Then **T6.2 to
T6.5** are independent files and can go to separate agents; T6.4 and T6.5 need M3.

Rule for every generator: the output, once written into the fixture workspace, must produce
no lint findings at warning level or above, and must parse with no `Unknown` statements. Each
task tests this.

---

## T6.1 Engine and the `scaffold` tool

**Depends on:** T1.3, T1.5, T2.5. **Owns:** `src/scaffold/mod.rs`,
`src/mcp/tools/scaffold.rs`

- `Plan { files: Vec<PlannedFile { path, content, action: Create | Modify { before_sha } }> , notes }`.
  Generators return a `Plan`; the engine renders or applies it.
- Dry run output: one row per file (action, pointer-style path, line count), then the full
  content of each file, within the 8 KB cap (larger plans go to a handle).
- Apply rules: target layer must be `writable`; paths pass `Workspace::confine`; never
  overwrite an existing file on `Create`; on `Modify`, the file's current hash must equal
  `before_sha` (else error: the file changed, re-run); write to a temp file and rename.
- After writing, refresh the index and run lint on the new files; include findings in the
  result.
- Dispatch on `kind` to the generators; unknown options are an error listing valid ones.

**Done when:** tests for dry run vs write, refusal on non-writable layer, path escape,
overwrite, stale `before_sha`, and the post-write lint.

## T6.2 Recipes and bbappends

**Depends on:** T6.1 (T3.4 for bbappend defaults). **Owns:** `src/scaffold/recipe.rs`,
`src/scaffold/append.rs`

`recipe`:
- Options: `name`, `version`, `category` (the `recipes-<category>` directory; default asks
  for it by error if the layer has several and none is obvious), `source` (git URL with
  branch and rev, tarball URL with checksum, or local files), `build` (`autotools`, `cmake`,
  `meson`, `cargo`, `setuptools`/python build backend, `go`, `kernel-module`, `none`),
  `license` + `lic_files_chksum`, `depends`, `rdepends`.
- Path `recipes-<category>/<pn>/<pn>_<pv>.bb`; variables in the OE style-guide order;
  correct `inherit` for the build system (check the class exists in the workspace and which
  name the release uses); `S` set correctly for git sources for the layer's release; package
  overrides in colon syntax; no placeholders that would break a parse. Missing facts that
  cannot be defaulted (license checksum, source hash) are listed in `notes` as things the
  user must supply, and marked in the file with a comment.
- If other versions of the recipe exist with a `.inc`, generate a `.bb` that requires it.

`bbappend`:
- Options: `recipe`, `version` (`exact`, `wildcard` giving `<pn>_%.bbappend`, or a prefix
  such as `1.%`), `set` (variables and operations), `files` (patches or files to add),
  `machine` (make the changes machine-conditional).
- Mirrors the recipe's directory structure in the target layer
  (`recipes-<same category>/<pn>/`).
- Adds `FILESEXTRAPATHS:prepend := "${THISDIR}/${PN}:"` when files are added, puts files
  under `<pn>/`, appends to `SRC_URI` with `:append` and a leading space, uses
  `:append:<machine>` forms rather than `:<machine> +=`.
- If the layer already has a bbappend for that recipe, produce a `Modify` plan that adds to
  it instead of creating a second file.

**Done when:** snapshot tests for each build system and source type; bbappend tests for each
option, for the existing-append case; generated files pass the parse and lint rule.

## T6.3 Layers and classes

**Depends on:** T6.1. **Owns:** `src/scaffold/layer.rs`, `src/scaffold/class.rs`

`layer`:
- Options: `name` (`meta-...`), `path`, `priority`, `depends`, `compat` (default: the compat
  series of the core layer in the workspace).
- Creates `conf/layer.conf` (`BBPATH`, `BBFILES` for `.bb` and `.bbappend`,
  `BBFILE_COLLECTIONS`, `BBFILE_PATTERN`, `BBFILE_PRIORITY`, `LAYERDEPENDS`,
  `LAYERSERIES_COMPAT`), `README.md`, `COPYING.MIT` only if asked, and an empty
  `recipes-example/` only if asked.
- A new layer is created outside existing layers, so the confinement rule here is "inside the
  directory that holds the workspace's layers or the configured path"; state this in the
  plan notes. Adding it to `bblayers.conf` is a separate `Modify` in the plan and notes that
  the config file must list it as writable to be used by later scaffolds.

`class`:
- Options: `name`, `scope` (`recipe` or `global`), `tasks`, `variables` (interface with
  defaults).
- Location `classes-recipe/` or `classes-global/` when the layer's compat supports it, else
  `classes/`.
- Interface variables with `?=`, task functions named `<class>_do_<task>` plus
  `EXPORT_FUNCTIONS`, `addtask` with ordering, a header comment documenting the interface.

**Done when:** snapshot tests; a generated layer is discovered by `Workspace::discover` when
added to the fixture `bblayers.conf`; a generated class is found by `find_class`.

## T6.4 `.inc` split

**Depends on:** T6.1, T3.4. **Owns:** `src/scaffold/inc_split.rs`

Given a recipe with two or more versions in a writable layer:

- Compute the statements common to all versions (same normalised text, same relative order).
- Version data stays in each `.bb`: `PV`, `SRCREV`, checksums, `LIC_FILES_CHKSUM`,
  version-specific patches in `SRC_URI`.
- Plan: create `<pn>.inc` with the common statements, and modify each `.bb` to
  `require <pn>.inc` followed by what remains. Preserve comments attached to moved
  statements and the original order.
- Safety check: evaluate each version before and after (apply the plan to an in-memory
  overlay) and compare every variable's final value and the task list. Any difference aborts
  the plan with the differing variables listed.

**Done when:** tests on fixture `hello_1.0` / `hello_1.1` variants: a clean split; a case
where order matters and the split is refused; the before/after equality check.

## T6.5 Placement advice (`kind: "where"`)

**Depends on:** T6.1, T3.4. **Owns:** `src/scaffold/placement.rs`

Writes nothing. Input: a recipe (optional), a variable or an intent (`add-patch`,
`change-config`, `add-package-to-image`, `set-machine-feature`, `pin-version`,
`change-provider`). Output: where the change belongs, why, and the `scaffold` call that would
make it.

Rules:
- Changes to an upstream recipe go in a bbappend in a writable layer, never in the upstream
  layer; prefer the existing bbappend if one exists.
- Hardware-specific values go in the machine conf, or a `:<machine>` override in the bbappend
  when recipe-specific.
- Policy (`DISTRO_FEATURES`, `PREFERRED_PROVIDER_*`, `PREFERRED_VERSION_*`, init system,
  libc) goes in the distro conf.
- Image content goes in the image recipe or a bbappend to it, not `local.conf`.
- `local.conf` is for developer-local, temporary settings only; say so when suggesting it.
- Logic shared by several recipes goes in a bbclass; data shared by versions of one recipe in
  a `.inc`.
- With several writable layers, pick by existing content (the layer that already appends
  this recipe or holds this machine/distro) and say how the choice was made.

**Done when:** table-driven tests for each intent on the fixture workspace.

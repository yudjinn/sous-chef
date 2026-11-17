# Lint rules

Each rule is one file `src/lint/rules/<id with / and - replaced by _>.rs` (e.g.
`override/legacy-syntax` is `override_legacy_syntax.rs`), implements the `Rule`
trait (M4, T4.1), has a positive and a negative fixture, and can be disabled by id or group
in `sous-chef.toml`.

Scope tells the engine what to hand the rule: `file` (one parsed file), `recipe` (the
evaluated recipe tree), `layer` (index queries over a layer).

Severity: **E** error (will break or is no longer accepted), **W** warning (likely bug or
convention breach), **I** info (style).

Releases: there is one rule set for all supported Yocto releases (Kirkstone to Wrynose). A
rule whose subject exists only in some releases declares that range (`Rule::releases`,
default: all), and the engine runs it for a file or layer only when the range includes one
of the releases it is checked against (`LintContext::releases`, oldest first): for a
writable layer, every supported release in its `LAYERSERIES_COMPAT` plus the workspace's
release; for a layer that is not writable, and for a file outside every layer (the build
directory's `conf/*.conf`), the workspace's release alone. A finding that holds for only
some of them names them. See "Release contract" in architecture.md. A user changes
behaviour with `release`, `[lint] disable` and `[lint.severity]` in `sous-chef.toml`; there
are no user-defined rules.

## Running lint

`sous-chef lint [target] [--rule ID]... [--severity S] [--fail-on S] [--format F] [--all]`
and the `lint` tool take the same target: a layer name (`build` for the build directory),
a pointer `layer:path` or `layer:path:A-B` (the range keeps only findings in it), a path to
a file or directory relative to the working directory, or a recipe `pn` or `pn_pv` (its
`.bb` as BitBake picks it, the files it requires or includes, its bbappends; not classes).
With no target, the whole workspace: the writable layers and the build directory's
configuration files. Layers that are not writable are skipped, with a note, unless
`--all`; if no layer is writable, every layer is linted. A named layer, recipe or file is
linted whatever its layer. For a recipe or file target, layer-scope rules run on the layers
the files are in and only findings in those files are kept.

Which rules run:

- With no selection, every rule except those in `[lint] disable` and those off by default
  (`Rule::default_enabled` false).
- `--rule` / `rules` names ids or groups; an unknown one is an error. A rule named by its
  exact id runs even when disabled or off by default; a group selects only rules that are
  enabled.
- `style/variable-order` is an alias of `hygiene/variable-order`, and the group `style`
  names it too, wherever an id or group is accepted.
- Unknown ids or groups in `[lint] disable` or `[lint.severity]` do not stop lint; each
  becomes a note in the report.
- `[lint.severity]` replaces a rule's severity; an entry for the id wins over one for its
  group.

Inline suppression: a comment `# sous-chef: allow <id-or-group>[, <id-or-group>...]` on the
line directly above the finding's line, or directly above the statement that contains it.
Several such comment lines may be stacked.

Exit status of `sous-chef lint`: 0 clean, 1 a reported finding at or above `--fail-on`
(default `error`), 2 lint could not run (bad target, unknown rule id, unreadable
workspace). Findings hidden by `--severity` never fail the run.

## Group `override` (task T4.2)

| Id | Sev | Scope | Flags |
|---|---|---|---|
| `override/legacy-syntax` | E | file | Underscore override syntax (`VAR_append`, `RDEPENDS_${PN}`). Use the detection rules in bitbake-semantics.md; must not flag `do_install`, `SRCREV_machine`, `BBFILE_PRIORITY_x` and similar. |
| `override/append-no-space` | W | file | `:append` value without a leading space, or `:prepend` without a trailing space, on a space-separated list variable. Not flagged for function bodies or for variables known to be non-lists (paths with `:` separators such as `FILESEXTRAPATHS`). |
| `override/plus-equals-with-override` | W | file | `VAR:foo += "x"` (and `=+`, `.=`, `=.`): this modifies `VAR:foo`, replacing `VAR` when `foo` is active, rather than appending to `VAR`. Suggest `VAR:append:foo`. |
| `override/remove-in-recipe` | W | file | `:remove` in a `.bb`, `.inc` or `.bbclass`: it cannot be undone by a bbappend or conf. Acceptable in distro/machine/local conf and bbappends (info there). |
| `override/dead` | W | recipe | Override that is in no known machine, distro, class, package or `OVERRIDES` entry in the workspace. |
| `override/shadowed` | I | recipe | A plain assignment that is always overwritten by a later unconditional `=` in the same tree. |
| `override/weak-after-default` | I | file | `??=` for a variable that already has `?=` or `=` earlier in the same file. |
| `override/missing-package` | E | file | `RDEPENDS`, `RRECOMMENDS`, `FILES`, `pkg_postinst` and other package-scoped variables assigned without a package override (`:${PN}`). |
| `override/append-vs-plus-equals` | I | file | `:append` in a `.bb` where the variable is set with `=` earlier in the same file and `+=` would do. Not flagged in bbappends or classes, where `:append` is usually right. |
| `override/plus-equals-on-default` | W | file | `VAR += ` in a conf file for a variable that is given its value with `?=` elsewhere in the workspace: the default is lost. Suggest `:append`. |
| `override/filesextrapaths` | E | file | `FILESEXTRAPATHS` modified without `:prepend`/`:append`, without `:=`, or without the trailing/leading colon. |

## Group `org` (task T4.3)

| Id | Sev | Scope | Flags |
|---|---|---|---|
| `org/recipe-path` | W | layer | Recipe not under `recipes-<category>/<pn>/`, or file name not `<pn>_<pv>.bb`, or upper-case / underscore in `PN`. |
| `org/duplicate-versions` | I | layer | Two or more versions of a recipe whose bodies are mostly identical: suggest a shared `<pn>.inc`. |
| `org/inc-has-version-data` | W | file | A `.inc` shared by several versions sets `PV`, `SRCREV`, `SRC_URI[sha256sum]` or `LIC_FILES_CHKSUM`. |
| `org/inc-unused` | I | layer | A `.inc` that nothing requires or includes. |
| `org/include-must-exist` | W | file | `include` of a file the recipe cannot work without (it exists and defines `SRC_URI`, `LICENSE` or tasks): use `require`. Also `require` of a file that does not resolve. |
| `org/repeated-logic` | I | layer | The same function body or block of assignments in three or more recipes of a layer: suggest a bbclass or `.inc`. |
| `org/class-recipe-specific` | W | file | A bbclass that sets `PN`-specific values (`SRC_URI`, `PV`, `LICENSE`) or uses `=` where `?=` would let recipes override. |
| `org/class-export-functions` | W | file | A bbclass defines `<class>_do_<task>` without `EXPORT_FUNCTIONS`, or defines `do_<task>` directly (overwriting recipe tasks). |
| `org/class-location` | I | layer | Class in `classes/` in a layer whose compat is 4.1 or later: suggest `classes-recipe/` or `classes-global/`. |
| `org/inherit-defer` | I | file | `inherit ${VAR}` where the variable may be changed later (by a bbappend or anonymous python): suggest `inherit_defer` if the layer's compat supports it. |
| `org/append-no-match` | W | layer | A bbappend that matches no recipe in the workspace. |
| `org/append-wildcard-broad` | I | layer | `<pn>_%.bbappend` that matches several versions while its content pins version-specific data (patches, `SRCREV`). |
| `org/append-missing-filesextrapaths` | E | file | A bbappend adds `file://` to `SRC_URI` but never extends `FILESEXTRAPATHS`. |
| `org/policy-in-recipe` | W | file | A recipe or bbappend sets distro or machine policy (`DISTRO_FEATURES`, `MACHINE_FEATURES`, `PREFERRED_PROVIDER_*`, `IMAGE_INSTALL`). |
| `org/recipe-change-in-local-conf` | I | file | `local.conf` carries `:pn-<recipe>` changes that belong in a bbappend or distro conf. |
| `org/layer-conf` | E/W | layer | Missing `LAYERSERIES_COMPAT`, missing `LAYERDEPENDS` while using classes from another layer, collection name clash, `BBFILES` that misses `.bbappend`, `BBPATH` not extended with `LAYERDIR`. |
| `org/upstream-edit` | W | layer | Uncommitted modifications inside a layer that is not marked writable (checked with `git status` if the layer is a git checkout). |

## Group `hygiene` (task T4.4)

| Id | Sev | Scope | Flags |
|---|---|---|---|
| `hygiene/missing-metadata` | W | recipe | No `SUMMARY`, `LICENSE` or `LIC_FILES_CHKSUM` (unless `LICENSE = "CLOSED"`); no `HOMEPAGE` is info. |
| `hygiene/autorev` | W | file | `SRCREV = "${AUTOREV}"` outside a `local.conf`. |
| `hygiene/src-uri-checksum` | W | recipe | Remote tarball in `SRC_URI` without a `sha256sum`; git URL without `SRCREV` or with `branch` missing. |
| `hygiene/host-path` | W | file | Absolute host paths (`/usr/lib`, `/home/...`) in task bodies instead of `${D}`, `${S}`, `${B}`, `${STAGING_*}`, `${bindir}` and friends; `do_install` that never references `${D}`. |
| `hygiene/workdir-s` | I | file | `S = "${WORKDIR}"` (not allowed in recent releases; verify the release that changed it). |
| `hygiene/variable-order` | I | file | Order differs from the OE style guide (description block, license, dependencies, `SRC_URI`, `S`, inherit, `PACKAGECONFIG`, tasks, packaging). Off by default: the rule returns false from `default_enabled` (T4.4 adds this; the M0 stub does not have it yet), so it runs only when selected by its id or the alias `style/variable-order`. |
| `hygiene/tabs` | I | file | Tabs in shell task bodies mixed with spaces; python bodies not indented with 4 spaces. |
| `hygiene/continuation` | W | file | A comment line or a blank line inside a continued assignment; trailing whitespace after `\`. |
| `hygiene/parse-error` | E/W | file | Any `Unknown` statement no parser diagnostic covers, and parser diagnostics. Not reported: whitespace after a trailing `\` (left to `hygiene/continuation`), `addfragments`, and `addpylib` in a `.conf`. Warning: the `addtask` quirk that joins two `before`/`after` groups into one task name. Error: everything else. In a `.conf` file, a function, `def`, `inherit`, `inherit_defer`, `addtask`, `deltask`, `addhandler` or `EXPORT_FUNCTIONS` is an error (BitBake's configuration parser rejects them). |

## Output

A summary line first (counts by severity and by rule), then one row per finding:
`severity rule pointer message`, then the notes (skipped layers or rules, configuration
problems, evaluation failures). The message says what is wrong and what to write instead,
in one line. Findings are sorted by file (layer, path), then line, rule and message; exact
duplicates are removed.

```
4 findings in 6 files: 3 error, 1 warning, 0 info; by rule: hygiene/parse-error 4
severity	rule	pointer	message
error	hygiene/parse-error	meta-mine:conf/distro/mine.conf:1	`inherit` in a .conf file: BitBake's configuration parser rejects it as an unparsed line; move it to a class or recipe (use INHERIT += for a global class)
error	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:3	unparsed line; fix the line so BitBake can parse it, or remove it
warning	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:4	addtask: `before`/`after` groups split by the other keyword are concatenated by BitBake without a space, joining two task names; write all `after` tasks in one group and all `before` tasks in another
error	hygiene/parse-error	meta-mine:recipes-app/bad/bad_1.0.bb:5-6	function do_broken is never closed: no `}` in column 0 before end of file; close the function with `}` in column 0
```

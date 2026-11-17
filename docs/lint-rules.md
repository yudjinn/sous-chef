# Lint rules

Each rule is one file `src/lint/rules/<id with / replaced by _>.rs`, implements the `Rule`
trait (M4, T4.1), has a positive and a negative fixture, and can be disabled by id or group
in `sous-chef.toml`.

Scope tells the engine what to hand the rule: `file` (one parsed file), `recipe` (the
evaluated recipe tree), `layer` (index queries over a layer).

Severity: **E** error (will break or is no longer accepted), **W** warning (likely bug or
convention breach), **I** info (style).

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
| `hygiene/variable-order` | I | file | Order differs from the OE style guide (description block, license, dependencies, `SRC_URI`, `S`, inherit, `PACKAGECONFIG`, tasks, packaging). Ships disabled-by-default as `style/variable-order` alias. |
| `hygiene/tabs` | I | file | Tabs in shell task bodies mixed with spaces; python bodies not indented with 4 spaces. |
| `hygiene/continuation` | W | file | A comment line or a blank line inside a continued assignment; trailing whitespace after `\`. |
| `hygiene/parse-error` | E | file | Any `Unknown` statement or parser diagnostic. |

## Output

One row per finding: `severity  rule-id  pointer  message`. The message says what is wrong
and what to write instead, in one line. Findings are sorted by file, then line.

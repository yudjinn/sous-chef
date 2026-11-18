---
name: yocto-layers
description: Yocto / OpenEmbedded / BitBake layers (.bb, .bbappend, .inc, .bbclass, .conf, kas files) with the sous-chef MCP tools, instead of grepping and reading files.
---

# Yocto layers with sous-chef

Ask sous-chef before grep, find or Read: it follows BitBake's parse order, layer priority
and override chain, and it sees layers that `.gitignore` hides from Grep and Glob.

| Question | Tool |
|---|---|
| A recipe: file, versions, bbappends, classes, sources, tasks, packages, CVE inputs | `recipe` |
| Final value of a variable and every operation on it | `var` |
| Active, conditional or misspelt overrides | `overrides` |
| The exact text of a function or assignment | `item` |
| Where a name or text occurs | `search` |
| A class: which file wins, its interface, who inherits it | `class` |
| Layers, shadowed files, bbappend matches, compatibility | `layers` |
| Dependencies, reverse dependencies, providers | `deps` |
| Convention and release problems | `lint` |
| New recipe, bbappend, layer, class, `.inc` split; where a change belongs | `scaffold` |
| Run bitbake, devtool, bitbake-layers (background job) | `bitbake` |
| Why a task failed | `log` |
| Read stored output `h:<id>` | `slice` |
| Workspace, release, index, build environment | `status` |
| Review of the whole repository | `audit` |
| What changed upstream between two revisions, effect on your bbappends | `diff` |
| Why a task re-ran | `sigs` |
| Which package ships a file (needs a build) | `pkgdata` |

- Answers are pointers `layer:path:A-B`; expand the ones you need with `item`, whose text
  is usable as Edit `old_string`. `+N more` means the answer was cut: the note says how to
  get the rest.
- Values are static. Allowlisted python helpers are evaluated; anything else in `${@...}`
  and anonymous python is not, and the answer says so: then use `var` with
  `authoritative: true` (runs `bitbake -e`), or say the value is uncertain.
- A note `no MACHINE is set` means pass `machine`. Multiconfig: pass `mc`.
- **`lint` checks conventions and release compatibility, not whether the build works.** No
  finding is not a clean bill of health: its note says what was not checked. When asked
  what is wrong with a layer, also read its recipes.
- Change an upstream recipe through a bbappend in a writable layer, not by editing it. In
  a kas project, change configuration in the kas file, not the generated `local.conf` or
  `bblayers.conf`.
- `scaffold` and `lint fix` are dry runs unless `write: true`.

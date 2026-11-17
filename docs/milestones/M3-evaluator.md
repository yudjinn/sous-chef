# M3: Evaluator

The part that makes sous-chef more than a search index: replay files in BitBake's order,
record every operation, apply the override chain, and explain the result.

Read first: [bitbake-semantics.md](../bitbake-semantics.md) in full, the evaluator contract
in [architecture.md](../architecture.md).

Parallelism: **T3.1**, **T3.2** can be written at the same time (different files, contract
fixed). **T3.3** needs T3.1 and T3.2. **T3.4** needs T3.1 and M2's query layer. The three
tools (**T3.5 to T3.7**) can go to separate agents once T3.4 is merged.

Accuracy rule for the whole milestone: when the static result may be wrong, say so in the
`Value` (`unevaluated_python`, `unresolved`). Never present a guess as a fact.

---

## T3.1 Datastore with history

**Depends on:** M0. **Owns:** `src/eval/datastore.rs`

- Store, per variable name as written (`VAR`, `VAR:foo`, `VAR:append:foo`), the ordered list
  of `VarOp`s, plus flags.
- Apply parse-time operators as they arrive: `=`, `?=` (only if no value yet), `:=` (expand
  now through a callback), `+=`, `=+`, `.=`, `=.`; record `??=` separately (last one wins,
  applied only if nothing else set the variable). `unset` clears.
- `:append`, `:prepend`, `:remove` are stored, not applied.
- Each recorded op keeps its `Loc` and the `via` chain supplied by the replay.
- `history(base)` returns ops for the base name and all its override variants, in order.
- Cheap copy-on-write fork (`DataStore::fork`) so the configuration store can be evaluated
  once and reused for every recipe.

**Done when:** unit tests for each operator, the `?=` / `??=` interactions, `+=` on an unset
variable, `unset`, and fork isolation.

## T3.2 Expansion

**Depends on:** M0. **Owns:** `src/eval/expand.rs`

- Expand `${VAR}` recursively against a lookup closure, with cycle detection (a cycle leaves
  the reference in place and records it in `unresolved`).
- `${@...}`: left as text, `unevaluated_python = true`. Exception list, kept small and tested:
  none in the first version. (Do not add "simple" python evaluation without agreement.)
- Undefined variables stay as `${VAR}` and are listed in `unresolved`, as BitBake does.
- Expansion of names (`FILES:${PN}-dev`) uses the same function.

**Done when:** unit tests for nesting, cycles, undefined, python, and name expansion.

## T3.3 Override finalisation

**Depends on:** T3.1, T3.2. **Owns:** `src/eval/overrides.rs`

- Compute `OVERRIDES` by expanding the variable from the store; split on `:`; drop empties.
- `DataStore::value(name)`: pick the winning variant (highest-priority active override, with
  chained overrides requiring all elements active), apply active `:append`, `:prepend`,
  `:remove` per bitbake-semantics.md, then expand.
- Produce the `OpStatus` for every op in `history`: `Applied`, `InactiveOverride`, `Shadowed`
  (overwritten by a later `=` or lost to a higher-priority variant), `Removed`.
- Confirm the *verify* item on append/prepend ordering against BitBake's `data_smart.py` and
  fix the doc.

**Done when:** table-driven tests, one per row of the "Overrides" block in the semantics doc,
plus: two active variants with different priority; chained overrides; `:remove` of an item
added by `:append`; `VAR:foo += "x"` behaving as documented; `:append:foo` with `foo`
inactive.

## T3.4 Replay

**Depends on:** T3.1, T2.3, T1.4. **Owns:** `src/eval/replay.rs`, `src/eval/mod.rs`

`evaluate(index, ws, target)`:

1. **Configuration pass** (cached per workspace + machine + distro, invalidated when any conf
   or global class file changes): `bblayers.conf`, each `layer.conf` (with `LAYERDIR` set),
   `bitbake.conf` following its includes as written, then `INHERIT` classes and
   `base.bbclass`. `MACHINE` / `DISTRO` from the target override the configured ones before
   the machine and distro confs are included.
2. **Recipe pass** on a fork: set `FILE`, `PN`, `PV`, `BPN`, `BP`, `THISDIR`,
   `FILE_LAYERNAME` from the file; feed the `.bb` statements in order; at each `require` /
   `include` / `inherit` resolve the target with the current store (so `${PN}`-style paths
   work) and recurse, pushing the statement onto the `via` chain. Guard against cycles and
   double inherits (a class is inherited once).
3. bbappends in layer priority order (confirm the *verify* item).
4. Deferred inherits. Anonymous python is not run; record that the recipe has it.
5. Record `FileVisit { loc, kind, via }` for every file in visit order.

Statements are read from the index (`statements.value`), not by reparsing files.

Functions are stored like variables (name with overrides, body pointer); `value()` for a
function reports the composed pieces as pointers rather than concatenated text.

**Done when:**
- Fixture tests assert the exact visit order for `hello_1.1` (conf files, classes, `.inc`,
  both bbappends in priority order).
- `SRC_URI` history for `hello` shows operations from the recipe, the `.inc` and each
  bbappend with correct `via` chains and statuses, for two different machines.
- A recipe with `${@...}` and anonymous python evaluates without error and flags the affected
  values.
- Evaluating a second recipe reuses the cached configuration pass (assert via a counter).

## T3.5 Tool: `var`

**Depends on:** T3.3, T3.4, T2.4, T2.5. **Owns:** `src/mcp/tools/var.rs`

Per mcp-tools.md. Static mode only here; `authoritative: true` returns
`Error::BitbakeUnavailable` with a message until T5.5 fills it in (leave a clearly marked
hook function for T5.5 to implement in its own file).

- Name handling: `FILES:${PN}` is expanded for the recipe before lookup.
- Long final values are truncated to 600 characters; the full value goes to the handle store
  if T5.1 is merged, otherwise the truncation marker gives the length.
- Global mode (`recipe` omitted) evaluates the configuration pass only.

**Done when:** snapshot tests for a list variable with appends from three layers, a variable
with machine-conditional values under two machines, a flag query, and a python-dependent
value.

## T3.6 Tool: `overrides`

**Depends on:** T3.3, T3.4. **Owns:** `src/mcp/tools/overrides.rs`

Per mcp-tools.md: `chain`, `conditional`, `dead`.

- `chain`: for each entry, the pointer of the assignment that contributes it
  (`MACHINEOVERRIDES` in the machine conf, and so on).
- `dead`: an override is known if it appears in `OVERRIDES` for any machine or distro conf in
  the workspace, is a class override, a `pn-` or `layer-` form, or a package name of the
  recipe. Everything else is reported with the closest known name (edit distance) as a hint.

**Done when:** snapshot tests, including a misspelled machine override in the fixtures that
is reported with the right suggestion.

## T3.7 Tool: `deps` and provider resolution

**Depends on:** T3.4. **Owns:** `src/mcp/tools/deps.rs`, `src/eval/providers.rs` (new file,
declared in `eval/mod.rs` by M0 as a stub)

- `providers.rs`: providers of a name from the index (`provides`) refined by evaluation
  (`PROVIDES` with expansion), selection by `PREFERRED_PROVIDER_<name>`,
  `PREFERRED_VERSION_<pn>` (with `%`), `DEFAULT_PREFERENCE`, then layer priority and version.
  Update `preferred_recipe` callers to use it.
- `deps` per mcp-tools.md: `build`, `runtime` (per package), `packageconfig` (option, enable
  and disable arguments, build deps, runtime deps, whether selected), `reverse` (from the
  index `depends` table; note it is static), `providers`.
- Class-contributed dependencies appear with the class as their pointer.

**Done when:** snapshot tests on the fixtures: `virtual/foo` resolves to the provider chosen
in the distro conf, with the pointer; `PACKAGECONFIG` table for `libfoo`; reverse
dependencies of `libfoo`.

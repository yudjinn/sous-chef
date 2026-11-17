# M0: Foundation

**Serial. One agent. Must be merged before anything else starts.**

The goal is a crate that compiles, with every module, tool and lint rule present as a stub,
so that later tasks only fill in files they own and never collide.

Read first: [architecture.md](../architecture.md).

---

## T0.1 Crate skeleton and dependencies

**Owns:** `Cargo.toml`, `src/main.rs`, `src/lib.rs`, `.gitignore`, `rustfmt.toml`, `clippy.toml`

- Add every crate listed in architecture.md. Look up the current version and feature flags of
  each (particularly `rmcp`: stdio server transport and tool macros) in its documentation.
- Create the module tree from architecture.md with empty or stub modules.
- `main.rs`: clap CLI with subcommands `serve`, `index`, `lint`, `hook`, global flags
  `--build-dir`, `--config`, `--layer` (repeatable). Each subcommand calls into the library
  and currently returns "not implemented".

**Done when:** `cargo build`, `cargo clippy --all-targets -- -D warnings` and
`cargo fmt --check` pass; `sous-chef --help` lists the four subcommands.

## T0.2 Shared types and errors

**Owns:** `src/types.rs`, `src/error.rs`, `src/parse/ast.rs`

- Implement the types exactly as in architecture.md: `LineSpan`, `Loc` (with `Display` and
  `FromStr` for `layer:path:A-B`), `FileKind` (with `from_path`), `AssignOp` (with the
  operator text both ways), `VarRef`, `Statement`, `StatementKind`, `ParsedFile`, `Lang`,
  `IncludeMode`.
- `error::Error` with the variants in architecture.md, `Result<T>` alias.
- Derive `Debug`, `Clone`, `PartialEq`, `Eq`, and `Serialize` where it helps JSON output.

**Done when:** unit tests cover `Loc` round-trip (including a single-line span and a path
containing `:`-free unusual characters), `FileKind::from_path`, `AssignOp` text mapping.

## T0.3 Public signatures as stubs

**Owns:** the `mod.rs` of `parse`, `workspace`, `index`, `eval`, `bitbake`, `lint`,
`scaffold`, `mcp`; `src/render.rs`, `src/handles.rs`, `src/config.rs`, `src/hook.rs`

- Declare every public function and struct named in the contracts of architecture.md, with
  bodies that return `Err(Error::Invalid { msg: "not implemented" })` or `todo`-free
  equivalents. No `todo!()`/`unimplemented!()`; stubs must not panic.
- `src/mcp/tools/`: one file per tool in mcp-tools.md (14 files), each with an `Args` struct
  matching the documented arguments and a `run` returning "not implemented".
- `src/lint/rules/`: one file per rule id in lint-rules.md, each a unit struct implementing
  the `Rule` trait with an empty `check`. Define the `Rule` trait and `Finding` here:

  ```rust
  pub enum Scope { File, Recipe, Layer }
  pub enum Severity { Info, Warning, Error }
  pub struct Finding { pub rule: &'static str, pub severity: Severity, pub loc: Loc, pub message: String }
  pub trait Rule: Sync {
      fn id(&self) -> &'static str;
      fn scope(&self) -> Scope;
      fn default_severity(&self) -> Severity;
      fn check(&self, cx: &LintContext, out: &mut Vec<Finding>);
  }
  pub fn all_rules() -> &'static [&'static dyn Rule];
  ```

  `LintContext` is declared with the fields T4.1 will fill (parsed file, optional `DataStore`,
  `&Index`, `&Workspace`, the target `Loc`).
- Register all tools and rules in their registries now.

**Done when:** the crate compiles; a test asserts the tool registry has the 14 names in
mcp-tools.md and the rule registry has every id in lint-rules.md.

## T0.4 Fixtures

**Owns:** `tests/fixtures/**`, `tests/common/mod.rs`

Build a small synthetic workspace that exercises every feature. Keep files short. Add a
`tests/fixtures/README.md` that says what each file is there to test.

```
tests/fixtures/ws/
  build/conf/bblayers.conf            BBLAYERS with continuation lines and ${TOPDIR}
  build/conf/local.conf               MACHINE, DISTRO, a :pn-<recipe> override, a += on a ?= var
  meta-core/                          stands in for OE-Core
    conf/layer.conf                   priority 5
    conf/bitbake.conf                 OVERRIDES definition, includes local/machine/distro conf
    conf/machine/qemux86.conf  conf/machine/boardx.conf (with MACHINEOVERRIDES)
    conf/distro/testdistro.conf
    classes/base.bbclass
    classes-recipe/autotoolsish.bbclass   EXPORT_FUNCTIONS, addtask, ?= interface
    classes-global/sanityish.bbclass
    recipes-core/hello/hello_1.0.bb  hello_1.1.bb  hello.inc  files/hello.patch
    recipes-core/libfoo/libfoo_2.0.bb     PROVIDES virtual/foo, PACKAGECONFIG
  meta-vendor/                        priority 6
    conf/layer.conf
    recipes-core/hello/hello_%.bbappend   FILESEXTRAPATHS, :append, machine override
    recipes-vendor/libfoo-alt/libfoo-alt_1.0.bb   second provider of virtual/foo
  meta-mine/                          priority 10, writable
    conf/layer.conf
    recipes-core/hello/hello_1.1.bbappend
    recipes-app/app/app_0.1.bb        python task, anonymous python, ${@...}, fakeroot
    recipes-app/orphan/orphan_9.%.bbappend   matches nothing
  meta-legacy/                        NOT in bblayers; legacy "_" syntax samples
tests/fixtures/lint/                  one positive and one negative file per rule (filled by M4)
tests/fixtures/syntax/                one file per statement form, plus malformed files
```

`tests/common/mod.rs`: helpers `fixture_ws() -> Workspace`, `fixture_path(rel)`, and a helper
that copies the workspace to a temp dir for tests that write.

**Done when:** the fixture tree exists with the README; helpers compile; a smoke test reads
every fixture file as UTF-8.

## T0.5 CI and contributor notes

**Owns:** `.github/workflows/ci.yml` (or equivalent), `CONTRIBUTING.md`

- CI runs fmt, clippy with `-D warnings`, and tests.
- `CONTRIBUTING.md`: the rules from docs/README.md ("Rules for agents working in parallel"),
  how to run tests, how to add a tool, how to add a lint rule.

**Done when:** the workflow file is valid and the three commands pass locally.

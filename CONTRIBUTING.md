# Contributing to sous-chef

The plan lives in [docs/](docs/README.md). Read `docs/architecture.md` before writing code:
it is the contract between everyone working on the crate.

## Rules for agents working in parallel

(From [docs/README.md](docs/README.md).)

1. **Stay inside the files your task owns.** Each task lists them under *Owns*. M0 created a
   stub for every module, tool and lint rule, so you fill a file in; you do not add to a
   shared registry.
2. **Do not change `Cargo.toml`, `src/types.rs` or `src/error.rs`** without saying so in your
   hand-off. M0 added all dependencies. If you need a new one or a contract change, make the
   smallest change and name it explicitly so other agents can rebase.
3. **Program against the contracts in [architecture.md](docs/architecture.md)**, not against
   another agent's unfinished code. If a dependency is not merged yet, use the fixtures.
4. **Every task ends green:** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test`.
5. **Never guess BitBake behaviour.** If [bitbake-semantics.md](docs/bitbake-semantics.md)
   does not cover a case, check the BitBake user manual or the BitBake source, then add the
   rule to that doc in the same change.
6. **Report what you did not do.** A task hand-off lists skipped acceptance items and any
   known wrong behaviour.

## Building and testing

The toolchain is nightly (edition 2024).

```sh
cargo build
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test   # the gate
cargo run -- --help
```

- Unit tests sit next to the code (`#[cfg(test)] mod tests`).
- Integration tests are `tests/*.rs`; shared helpers are in `tests/common/mod.rs`
  (`mod common;` at the top of the test file): `fixture_path(rel)`, `fixture_ws()`,
  `fixture_config()`, `temp_ws()` (a writable copy of the fixture workspace),
  `files_under(dir)`.
- Fixtures are in `tests/fixtures/`; [tests/fixtures/README.md](tests/fixtures/README.md)
  says what each file is for and the expected answers. Update it when you change a fixture.
- Snapshot tests use `insta` (`cargo insta review` if you have `cargo-insta`, or accept by
  renaming the `.snap.new` files).
- No test may need a real BitBake or poky checkout, except the opt-in M8 tests gated on
  `SOUS_CHEF_POKY`.

## Conventions

- No `unwrap`, `expect`, `todo!`, `unimplemented!` outside tests (denied by
  `[lints.clippy]` in `Cargo.toml`; tests are exempt via `clippy.toml`; integration test
  files add `#![allow(clippy::unwrap_used, clippy::expect_used)]` if they need it).
- Library errors are `sous_chef::Error` (`thiserror`); `anyhow` only in `main.rs`.
- Logging goes to stderr. stdout belongs to the MCP transport (and to hook responses).
- Stubs left by M0 return `Err(Error::not_implemented())` and carry a `Stub (M0)` comment
  naming the task that fills them. Remove the comment (and any `#[allow(dead_code)]` it
  explains) when you implement the item.

## Adding or implementing an MCP tool

Each tool is one file, `src/mcp/tools/<name>.rs`:

```rust
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Args {
    /// Doc comments become the argument descriptions in the JSON schema.
    pub name: String,
    pub budget_tokens: Option<usize>,
    pub format: Option<Format>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<String>;
```

- Start with `let mut index = ctx.index.lock().await; let report = index.refresh(&ctx.workspace)?;`
  and render output through `crate::render` (`Table`, `freshness_line`).
- Errors are `Error` values with a one-line message saying what to do next; the server turns
  them into MCP tool errors.
- The future returned by `run` must be `Send` (the registry boxes it as such); the compiler
  will tell you if you hold something non-`Send` across `.await`.
- All 14 tools are already registered in `TOOLS` in `src/mcp/mod.rs`. A **new** tool needs
  three edits: the file, `pub mod <name>;` in `src/mcp/tools/mod.rs`, and one
  `tool!(<name>, Always|Deferred, "description")` row in `TOOLS`; then update
  `docs/mcp-tools.md` and the expected list in `tests/registry.rs`.

## Adding or implementing a lint rule

Each rule is one file, `src/lint/rules/<group>_<name>.rs` (the id with `/` and `-` replaced
by `_`; e.g. `override/legacy-syntax` is `override_legacy_syntax.rs`, struct
`OverrideLegacySyntax`), implementing `crate::lint::Rule`:

```rust
impl Rule for OverrideLegacySyntax {
    fn id(&self) -> &'static str { "override/legacy-syntax" }
    fn scope(&self) -> Scope { Scope::File }
    fn default_severity(&self) -> Severity { Severity::Error }
    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) { /* ... */ }
}
```

- All 37 rules in `docs/lint-rules.md` are already registered in `src/lint/rules/mod.rs`.
  A **new** rule needs the file, a `pub mod` line and an entry in `ALL` there, a row in
  `docs/lint-rules.md` (the registry test compares the two), and fixtures.
- Fixtures: `tests/fixtures/lint/<group>/<rule>/bad.*` and `good.*` (layer-scope rules:
  `bad-layer/` and `good-layer/`). The test asserts the exact findings on `bad` and none on
  `good`.
- Shared helpers go in `src/lint/helpers.rs` (owned by T4.1); otherwise keep helpers private
  to the rule file. False positives are worse than misses.

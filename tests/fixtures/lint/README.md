# Lint rule fixtures

Filled by M4 (T4.1 to T4.4). Layout, per docs/milestones/M4-lint.md:

- File and recipe scope rules: `tests/fixtures/lint/<group>/<rule>/bad.<ext>` and
  `good.<ext>`, e.g. `override/legacy-syntax/bad.bb`. The extension matters (rules check
  the file kind: `.bb`, `.bbappend`, `.inc`, `.bbclass`, `.conf`).
- Layer scope rules: small layers under `tests/fixtures/lint/<group>/<rule>/bad-layer/` and
  `good-layer/`, each with its own `conf/layer.conf`.

Each rule's test asserts the exact findings (rule id, line) on `bad` and none on `good`.
These files are never part of the fixture workspace in `tests/fixtures/ws/`.

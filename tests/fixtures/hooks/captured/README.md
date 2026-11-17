# Captured hook events

Real `PostToolUse` events, captured on 2026-11-17 from **Claude Code 2.1.326** (`claude -p`,
model haiku, `--tools Read,Grep` so that Grep is available on Linux) by a project hook that
appended its stdin to a file. The session ran in a copy of `tests/fixtures/ws`; that path is
replaced by `{{WS}}` and `transcript_path` by a placeholder. Nothing else is edited.

| File | Tool call |
|---|---|
| `grep_content.json` | Grep `install -d`, `path: "."`, `output_mode: "content"`, `-n: true` |
| `read_offset.json` | Read `hello.inc` with `offset: 10`, `limit: 6` |
| `read_whole_recipe.json` | Read `hello_1.1.bb`, whole file |

How they differ from the hand-written events beside this directory is recorded in
`plugin/README.md` ("Real events"). `tests/plugin_packaging.rs` runs them through
`sous-chef hook`.

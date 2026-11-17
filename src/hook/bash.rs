//! Hook handler that digests large bitbake output from Bash (T7.3).
//!
//! Trigger: `PostToolUse` on `Bash` whose `tool_input.command` runs `bitbake`, `devtool` or
//! `bitbake-layers` (the first word of any command in it, after variable assignments and
//! `sudo` / `time` / `env` / `nice` / `exec`), and whose output (`tool_response.stdout`
//! plus `stderr`) is larger than [`MIN_BYTES`]. Anything else passes through untouched.
//!
//! **Mechanism: the output is replaced** (`hookSpecificOutput.updatedToolOutput`). The
//! Claude Code hooks reference (2.1.326, recorded in `plugin/README.md`) documents that a
//! `PostToolUse` hook may replace a built-in tool's output when the value has the tool's
//! output shape; for Bash that is `{stdout, stderr, interrupted, isImage}`. Replacing is
//! what the digest is for: the full log no longer takes the context, and nothing is lost,
//! because the full output is stored first under a handle that the MCP server's `slice`
//! reads, and the replacement ends with the `slice` hint. Bash output is never the text an
//! Edit works on, so the "annotations are additions" rule does not apply to it. The
//! replacement keeps every other field of `tool_response` as it was (`interrupted`,
//! `isImage` and any others), puts the digest in `stdout` and empties `stderr`. If Claude
//! Code rejects the replacement the original output stays: it fails safe.
//!
//! The replacement text: one line naming the handle and the size, then the T5.6 digest
//! (`bitbake::logs::digest`, about 600 bytes, with the task definition from the index when
//! the console names a recipe and task) when `bitbake::logs::detect` finds a failure, or
//! else the last [`TAIL_LINES`] lines; the `slice` hint is the last line either way.
//!
//! **Handle store from a hook process.** The store is a directory plus rows in the index
//! database (`src/handles.rs`): `<cache dir>/handles` and the `handles` table of
//! `<cache dir>/index.sqlite`, the cache dir being the one [`Ctx::env`] found the index in.
//! The hook opens its own read-write connection to that existing database (never creating
//! it), with a short busy timeout, and opens the store with
//! [`HandleStore::without_retention`], so a short-lived process never sweeps blobs the
//! server is writing. The MCP server's `slice` then finds the row and the blob. A missing
//! index, a locked database or any write error fails open: no output, the tool result is
//! unchanged.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use super::{Ctx, Event, Handler, POST_TOOL_USE, Response};
use crate::bitbake::logs;
use crate::error::Result;
use crate::handles::{Handle, HandleKind, HandleStore};
use crate::render::{self, Format};

/// Output at or below this many bytes (stdout plus stderr) passes through untouched.
pub const MIN_BYTES: usize = 3 * 1024;

/// Lines shown when no failure is recognised.
pub const TAIL_LINES: usize = 10;

/// The programs whose output is digested.
const PROGRAMS: &[&str] = &["bitbake", "devtool", "bitbake-layers"];

/// Words that run the next word as the command.
const WRAPPERS: &[&str] = &["sudo", "time", "env", "nice", "exec", "command", "nohup"];

/// How long the hook waits for a write lock on the index database.
const WRITE_BUSY: Duration = Duration::from_millis(50);

/// Token budget of the rendered digest.
const DIGEST_TOKENS: usize = 400;

/// `PostToolUse` on `Bash`.
pub struct BashDigest;

/// Whether a shell command line runs one of [`PROGRAMS`]: the first word of any simple
/// command in it (split on `;`, `&`, `|`, newlines, parentheses and backquotes), after
/// `NAME=value` assignments and [`WRAPPERS`], compared by file name.
pub fn runs_bitbake(command: &str) -> bool {
    command
        .split([';', '&', '|', '\n', '(', ')', '`', '{', '}'])
        .any(|segment| {
            let first = segment
                .split_whitespace()
                .map(|w| w.trim_matches(|c| c == '"' || c == '\''))
                .find(|w| {
                    !(WRAPPERS.contains(w)
                        || (w.contains('=') && !w.starts_with('-'))
                        || w.starts_with('-') // options of a wrapper (`nice -n 5`)
                        || w.chars().all(|c| c.is_ascii_digit()))
                });
            first.is_some_and(|w| {
                let name = Path::new(w)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(w);
                PROGRAMS.contains(&name)
            })
        })
}

/// stdout, then stderr (on a new line) when there is any.
fn combined(stdout: &str, stderr: &str) -> String {
    if stderr.is_empty() {
        return stdout.to_string();
    }
    let mut s = String::with_capacity(stdout.len() + stderr.len() + 1);
    s.push_str(stdout);
    if !stdout.is_empty() && !stdout.ends_with('\n') {
        s.push('\n');
    }
    s.push_str(stderr);
    s
}

/// The text that replaces the output.
fn summary(text: &str, handle: &Handle, digest: Option<String>, with_stderr: bool) -> String {
    let lines = text.lines().count();
    let mut out = format!(
        "sous-chef: bitbake output ({lines} lines, {} bytes{}) stored as {handle}; the full \
         text is there, read it with the slice tool.\n",
        text.len(),
        if with_stderr {
            ", stdout then stderr"
        } else {
            ""
        }
    );
    match digest {
        Some(d) => out.push_str(&d),
        None => {
            out.push_str(&format!(
                "no failure recognised; last {} lines:\n",
                TAIL_LINES.min(lines)
            ));
            let tail: Vec<&str> = text.lines().rev().take(TAIL_LINES).collect();
            for l in tail.into_iter().rev() {
                out.push_str(&render::snippet(l));
                out.push('\n');
            }
            let last = lines.max(1);
            let first = last.saturating_sub(49).max(1);
            out.push_str(&format!(
                "slice handle={handle} grep=RE | lines={first}-{last}"
            ));
        }
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

impl Handler for BashDigest {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn events(&self) -> &'static [&'static str] {
        &[POST_TOOL_USE]
    }

    fn tools(&self) -> &'static [&'static str] {
        &["Bash"]
    }

    fn handle(&self, cx: &mut Ctx<'_>, ev: &Event) -> Result<Option<Response>> {
        let Some(command) = ev.input_str("command") else {
            return Ok(None);
        };
        let Some(resp) = ev.tool_response.as_object() else {
            return Ok(None);
        };
        if resp.get("isImage").and_then(Value::as_bool) == Some(true) || !runs_bitbake(command) {
            return Ok(None);
        }
        let stdout = resp.get("stdout").and_then(Value::as_str).unwrap_or("");
        let stderr = resp.get("stderr").and_then(Value::as_str).unwrap_or("");
        if stdout.len() + stderr.len() <= MIN_BYTES {
            return Ok(None);
        }
        let text = combined(stdout, stderr);

        let env = cx.env()?;
        let db = env.index.path().to_path_buf();
        let dir = env.index.cache_dir().join("handles");
        let conn = rusqlite::Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(WRITE_BUSY)?;
        let store = HandleStore::without_retention(&dir, conn)?;
        let handle = store.put(HandleKind::Build, command, text.as_bytes())?;
        env.check_budget()?;

        let digest = if logs::detect(&text).is_empty() {
            None
        } else {
            let meta = logs::meta_from_text(&text);
            let definition = match (&meta.recipe, &meta.task) {
                (Some(r), Some(t)) => logs::task_definition(env.index, env.ws, r, t),
                _ => None,
            };
            let d = logs::digest(&logs::DigestInput {
                text: &text,
                ws: Some(env.ws),
                handle: Some(&handle),
                definition: definition.as_ref(),
                ..logs::DigestInput::default()
            });
            Some(d.render(Format::Text, DIGEST_TOKENS))
        };
        let mut replaced = ev.tool_response.clone();
        if let Some(obj) = replaced.as_object_mut() {
            obj.insert(
                "stdout".into(),
                Value::String(summary(&text, &handle, digest, !stderr.is_empty())),
            );
            obj.insert("stderr".into(), Value::String(String::new()));
        }
        cx.note(format!("stored {} bytes as {handle}", text.len()));
        Ok(Some(Response {
            additional_context: None,
            updated_tool_output: Some(replaced),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_that_run_bitbake() {
        for c in [
            "bitbake hello",
            "  bitbake -c compile hello 2>&1 | tail -n 200",
            "source oe-init-build-env build && bitbake core-image-minimal",
            ". ./oe-init-build-env; MACHINE=qemux86 bitbake hello",
            "time bitbake hello",
            "nice -n 5 bitbake hello",
            "../poky/bitbake/bin/bitbake-layers show-layers",
            "devtool modify hello",
            "x=$(bitbake -e hello | grep ^S=)",
        ] {
            assert!(runs_bitbake(c), "{c}");
        }
        for c in [
            "cat tmp/log/cooker/qemux86/console-latest.log",
            "grep -r bitbake .",
            "echo bitbake",
            "bitbake-getvar -r hello S",
            "ls bitbake",
        ] {
            assert!(!runs_bitbake(c), "{c}");
        }
    }

    #[test]
    fn combined_output() {
        assert_eq!(combined("a", ""), "a");
        assert_eq!(combined("a", "b"), "a\nb");
        assert_eq!(combined("a\n", "b"), "a\nb");
        assert_eq!(combined("", "b"), "b");
    }
}

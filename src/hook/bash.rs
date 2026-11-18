//! Hook handler that digests large bitbake output from Bash (T7.3).
//!
//! Trigger: `PostToolUse` on `Bash` whose `tool_input.command` runs `bitbake`, `devtool` or
//! `bitbake-layers` (the first word of any command in it, after variable assignments and
//! `sudo` / `time` / `env` / `nice` / `exec` and their options, with the value of those
//! that take one, such as `env -u NETRC_FILE`; also `kas build`, `kas-container build` and a
//! `kas shell` / `kas-container shell` whose `-c` command runs one of them, see
//! [`runs_bitbake`]), and whose output (`tool_response.stdout`
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
//! Without a failure the warnings come first (`logs::console_warnings`, as the `bitbake`
//! tool shows a finished job's), and the tail leaves out knotty's count of them.
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

/// kas front ends: `kas` and the `kas-container` wrapper script.
const KAS_PROGRAMS: &[&str] = &["kas", "kas-container"];

/// Options before the kas or kas-container subcommand that take a value (kas 5.5
/// `kas.py` and the `kas-container` script).
const KAS_VALUE_OPTIONS: &[&str] = &[
    "-l",
    "--log-level",
    "--runtime-args",
    "--docker-args",
    "--ssh-dir",
    "--aws-dir",
    "--git-credential-store",
    "--git-credential-socket",
];

/// Whether a shell command line runs one of [`PROGRAMS`]: the first word of any simple
/// command in it (split on `;`, `&`, `|`, newlines, parentheses and backquotes), after
/// `NAME=value` assignments and [`WRAPPERS`], compared by file name. Also `kas build` /
/// `kas-container build` (kas runs BitBake), and `kas shell ... -c '<command>'` /
/// `kas-container shell ... -c ...` when the command runs one of them; other kas
/// subcommands (`checkout`, `dump`, `lock`, ...) do not count.
pub fn runs_bitbake(command: &str) -> bool {
    command
        .split([';', '&', '|', '\n', '(', ')', '`', '{', '}'])
        .any(|segment| {
            let words: Vec<&str> = segment
                .split_whitespace()
                .map(|w| w.trim_matches(|c| c == '"' || c == '\''))
                .collect();
            let Some(pos) = command_word(&words) else {
                return false;
            };
            let w = words[pos];
            let name = Path::new(w)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(w);
            if PROGRAMS.contains(&name) {
                return true;
            }
            KAS_PROGRAMS.contains(&name) && kas_runs_bitbake(&words[pos + 1..], segment)
        })
}

/// Options of the [`WRAPPERS`] that take the next word as their value, so that word is not
/// the command: `env -u NETRC_FILE kas ...` (how this project's live environment runs kas,
/// docs/live-environment.md), `env -C <dir>`, `sudo -u <user>`, `nice -n 5`, `time -f
/// <format>`.
const WRAPPER_VALUE_OPTIONS: &[&str] = &[
    "-u",
    "--unset",
    "-C",
    "--chdir",
    "-g",
    "--group",
    "--user",
    "-D",
    "-h",
    "--host",
    "-p",
    "--prompt",
    "-r",
    "--role",
    "-t",
    "--type",
    "-U",
    "--other-user",
    "-T",
    "--command-timeout",
    "-n",
    "--adjustment",
    "-f",
    "--format",
    "-o",
    "--output",
];

/// The index of the command word of a simple command: the first word that is not a
/// `NAME=value` assignment, a [`WRAPPERS`] word, a wrapper's option (with its value, see
/// [`WRAPPER_VALUE_OPTIONS`]) or a number.
fn command_word(words: &[&str]) -> Option<usize> {
    let mut i = 0;
    let mut after_wrapper = false;
    while let Some(w) = words.get(i) {
        if WRAPPERS.contains(w) {
            after_wrapper = true;
            i += 1;
        } else if after_wrapper && WRAPPER_VALUE_OPTIONS.contains(w) {
            i += 2;
        } else if (w.contains('=') && !w.starts_with('-'))
            || w.starts_with('-')
            || w.chars().all(|c| c.is_ascii_digit())
        {
            i += 1;
        } else {
            return Some(i);
        }
    }
    None
}

/// After `kas` / `kas-container`: whether the subcommand builds, or is a `shell` whose
/// `-c` / `--command` runs BitBake. `segment` is the raw text, to read the quoted command.
fn kas_runs_bitbake(words: &[&str], segment: &str) -> bool {
    let mut i = 0;
    while let Some(w) = words.get(i) {
        if !w.starts_with('-') {
            break;
        }
        i += if KAS_VALUE_OPTIONS.contains(w) { 2 } else { 1 };
    }
    match words.get(i) {
        Some(&"build") => true,
        Some(&"shell") => {
            let Some(at) = segment.find("shell") else {
                return false;
            };
            let rest = &segment[at + "shell".len()..];
            [" -c ", " --command ", " --command="]
                .iter()
                .filter_map(|opt| rest.find(opt).map(|p| &rest[p + opt.len()..]))
                .any(|cmd| {
                    let cmd = cmd.trim_start().trim_start_matches(['"', '\'']);
                    runs_bitbake(cmd)
                })
        }
        _ => false,
    }
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
            // The warnings themselves, not knotty's count of them (y1-build: a real build
            // ended "Summary: There were 3 WARNING messages.").
            let (shown, total) = logs::console_warnings(text);
            let warnings = logs::warning_rows(&shown, total, Some(handle));
            if !warnings.is_empty() {
                out.push_str(&format!(
                    "{total} {}:\n",
                    if total == 1 { "warning" } else { "warnings" }
                ));
                for w in &warnings {
                    out.push_str(&render::snippet(w));
                    out.push('\n');
                }
            }
            let kept: Vec<&str> = text
                .lines()
                .filter(|l| !logs::is_warning_summary(l))
                .collect();
            out.push_str(&format!(
                "no failure recognised; last {} lines:\n",
                TAIL_LINES.min(kept.len())
            ));
            let tail: Vec<&str> = kept.iter().rev().take(TAIL_LINES).copied().collect();
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
    fn kas_commands_that_run_bitbake() {
        for c in [
            "kas build kas/kiss.yaml:kas/dogbonedark.yaml",
            "kas build kas/kiss.yaml --target kiss-image -c fetch 2>&1 | tail",
            "kas -l debug build kas.yml",
            "~/.venv/bin/kas build kas.yml",
            "kas-container build kas/kiss.yaml",
            "kas-container --ssh-dir ~/.ssh build kas.yml",
            "kas shell kas/kiss.yaml:kas/dogbonedark.yaml -c 'bitbake -p'",
            "kas shell kas.yml -c \"bitbake -e curl\" | grep ^S=",
            "kas shell -k kas.yml --command 'bitbake-layers show-layers'",
            "kas shell kas.yml --command='devtool status'",
            "kas-container shell kas.yml -c 'bitbake -p'",
            "cd yocto-kiss && kas build kas/kiss.yaml",
            // How docs/live-environment.md runs kas: `env -u NAME` takes a value (y1-build:
            // the hook passed this through before).
            "env -u NETRC_FILE ../venv/bin/kas shell ../scratch/kas-scratch.yaml -c 'bitbake curl'",
            "env -u NETRC_FILE -C yocto-kiss kas build kas/kiss.yaml",
            "sudo -u builder bitbake hello",
            "env --unset=NETRC_FILE bitbake hello",
        ] {
            assert!(runs_bitbake(c), "{c}");
        }
        for c in [
            "kas checkout kas/kiss.yaml:kas/dogbonedark.yaml",
            "kas dump kas/kiss.yaml",
            "kas dump --lock --inplace kas/kiss.yaml",
            "kas lock kas.yml",
            "kas shell kas.yml -c 'runqemu'",
            "kas shell kas.yml",
            "kas-container checkout kas.yml",
            "kas --version",
            "pip install kas",
            "echo kas build",
            "env -u NETRC_FILE ../venv/bin/kas dump kas/kiss.yaml",
            "env -u bitbake true",
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

//! `org/upstream-edit`: Uncommitted modifications inside a layer that is not marked writable.
//!
//! See docs/lint-rules.md and T4.3's notes. For a layer that is not writable, runs
//! `git rev-parse --show-prefix`, `git ls-files -- conf/layer.conf` (a layer whose
//! `layer.conf` is not tracked is not an upstream checkout, just a directory inside some
//! other repository) and `git status --porcelain=v1 -z --untracked-files=all -- .` in the
//! layer root (argv, never a shell; `GIT_DIR`, `GIT_WORK_TREE` and friends
//! removed from the environment; no optional locks, no prompts), each with a timeout of
//! [`TIMEOUT`]. Anything going wrong (git missing, not a git checkout, a timeout, a non-zero
//! exit) skips the layer silently: git never makes lint fail.
//!
//! Reported (one finding per path, line 1, at most [`MAX_FINDINGS`], then one finding on
//! `conf/layer.conf` with the count of the rest): tracked files that are modified, added,
//! deleted, renamed or copied (staged or not), and untracked files that are BitBake metadata
//! (`.bb`, `.bbappend`, `.inc`, `.bbclass`, `.conf`). Other untracked files (editor backups,
//! build output) are not reported. Paths are made relative to the layer root with the
//! prefix git reports, so a layer inside a larger checkout (`poky/meta`) works.
//!
//! Since lint skips layers that are not writable unless asked (`--all`, or naming the
//! layer), this rule runs only then.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::lint::{Finding, LintContext, Rule, Scope, Severity};
use crate::types::{FileKind, LineSpan, Loc};

pub struct OrgUpstreamEdit;

/// Per git invocation.
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// Findings listed one by one before the rest are counted.
pub const MAX_FINDINGS: usize = 20;

const GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

/// Runs git with `args` in `dir`; its stdout when it exits 0 within [`TIMEOUT`].
fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("--no-optional-locks")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    for v in GIT_ENV {
        cmd.env_remove(v);
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a thread so a large output cannot block the child while we wait.
    let reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < TIMEOUT => thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = reader.join().ok()?;
    status.filter(|s| s.success()).map(|_| out)
}

/// What one porcelain entry says, for the paths worth reporting.
fn describe(code: &[u8]) -> Option<&'static str> {
    match code {
        b"??" => Some("untracked"),
        b"!!" => None,
        [x, y] => {
            let any = |c: u8| *x == c || *y == c;
            Some(if any(b'D') {
                "deleted"
            } else if any(b'R') {
                "renamed"
            } else if any(b'C') {
                "copied"
            } else if any(b'A') {
                "added"
            } else if any(b'U') {
                "unmerged"
            } else {
                "modified"
            })
        }
        _ => None,
    }
}

/// `(path relative to the layer root, what happened)` for every reportable entry.
fn changes(root: &Path) -> Option<Vec<(PathBuf, &'static str)>> {
    let prefix = git(root, &["rev-parse", "--show-prefix"])?;
    // A layer that sits untracked inside some other checkout is not an upstream checkout.
    let tracked = git(root, &["ls-files", "--", "conf/layer.conf"])?;
    if tracked.iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    let prefix = String::from_utf8_lossy(&prefix).trim_end().to_string();
    let raw = git(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ],
    )?;
    let mut out = Vec::new();
    let mut entries = raw.split(|b| *b == 0).filter(|e| !e.is_empty());
    while let Some(e) = entries.next() {
        if e.len() < 4 {
            continue;
        }
        let (code, path) = (&e[..2], &e[3..]);
        // A rename or copy is followed by its source path, which is not reported.
        if code.contains(&b'R') || code.contains(&b'C') {
            entries.next();
        }
        let Some(what) = describe(code) else { continue };
        let path = String::from_utf8_lossy(path).into_owned();
        let rel = path.strip_prefix(prefix.as_str()).unwrap_or(&path);
        let rel = PathBuf::from(rel);
        if what == "untracked" && FileKind::from_path(&rel).is_none() {
            continue;
        }
        out.push((rel, what));
    }
    Some(out)
}

impl Rule for OrgUpstreamEdit {
    fn id(&self) -> &'static str {
        "org/upstream-edit"
    }

    fn scope(&self) -> Scope {
        Scope::Layer
    }

    fn default_severity(&self) -> Severity {
        Severity::Warning
    }

    fn check(&self, cx: &LintContext<'_>, out: &mut Vec<Finding>) {
        let Some(layer) = cx.layer.and_then(|n| cx.ws.layer(n)) else {
            return;
        };
        if layer.writable {
            return;
        }
        let Some(changed) = changes(&layer.root) else {
            return;
        };
        for (path, what) in changed.iter().take(MAX_FINDINGS) {
            out.push(Finding {
                rule: self.id(),
                severity: self.default_severity(),
                loc: Loc {
                    layer: layer.name.clone(),
                    path: path.clone(),
                    lines: LineSpan::line(1),
                },
                message: format!(
                    "{} is {what} in {}, a layer that is not writable (an upstream checkout): \
                     the change is lost on the next update; commit it upstream, or move it to a \
                     bbappend in your own layer (or list the layer in writable_layers)",
                    path.display(),
                    layer.name
                ),
            });
        }
        if changed.len() > MAX_FINDINGS {
            out.push(Finding {
                rule: self.id(),
                severity: self.default_severity(),
                loc: Loc {
                    layer: layer.name.clone(),
                    path: PathBuf::from("conf/layer.conf"),
                    lines: LineSpan::line(1),
                },
                message: format!(
                    "{} more uncommitted changes in {}, a layer that is not writable; see `git \
                     status` in {}",
                    changed.len() - MAX_FINDINGS,
                    layer.name,
                    layer.root.display()
                ),
            });
        }
    }
}

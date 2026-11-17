//! Read-only git commands, shared by `lint` (`org/upstream-edit`) and the `audit` report.
//!
//! [`git`] runs `git` with an argv (never a shell) in a directory and returns its stdout when
//! it exits 0 within [`TIMEOUT`]; anything else (git missing, not a repository, a non-zero
//! exit, a timeout) is `None`, so a caller never fails because of git. The command is made
//! safe to run in a checkout that belongs to someone else:
//!
//! - stdin closed, `GIT_TERMINAL_PROMPT=0`: no prompt can block it;
//! - `--no-optional-locks` and `GIT_OPTIONAL_LOCKS=0`: git takes no lock it does not need,
//!   so `git status` does not refresh and rewrite `.git/index` (taking `index.lock`) while
//!   the user, or their editor, works in the same checkout;
//! - `-c core.fsmonitor=false`: a repository that sets `core.fsmonitor` would otherwise
//!   start git's file system monitor daemon (`true`), which keeps running after the command,
//!   or run the hook program the setting names (a path), a program the repository chooses;
//! - the variables that point git at another repository, index, object store or
//!   configuration (`GIT_DIR`, `GIT_WORK_TREE`, ..., `GIT_CONFIG_PARAMETERS`) removed, so the
//!   command reads the repository that contains `dir`, as configured on disk;
//! - `LC_ALL=C`: output in a fixed language.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Per git invocation.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Removed from git's environment (module docs).
const GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
];

/// Runs `git <args>` in `dir` (module docs); its stdout when it exits 0 within [`TIMEOUT`].
pub fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
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

/// [`git`], with the output decoded as UTF-8 (invalid bytes replaced).
pub fn git_text(dir: &Path, args: &[&str]) -> Option<String> {
    git(dir, args).map(|out| String::from_utf8_lossy(&out).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_a_repository_or_on_failure_it_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        // GIT_CEILING_DIRECTORIES is removed, so make sure no parent is a repository by
        // asking for something that fails everywhere.
        assert_eq!(git(tmp.path(), &["no-such-git-subcommand"]), None);
        assert_eq!(git(&tmp.path().join("missing"), &["status"]), None);
    }

    #[test]
    fn version_runs_and_is_text() {
        // `git --version` needs no repository; skipped when git is not installed.
        let Some(out) = git_text(Path::new("."), &["--version"]) else {
            return;
        };
        assert!(out.starts_with("git version"), "{out}");
    }
}

//! Read-only git commands, shared by `lint` (`org/upstream-edit`), the `audit` report and
//! the release `diff`.
//!
//! [`run`] runs `git` with an argv (never a shell) in a directory, optionally feeding it
//! bytes on stdin, and returns its stdout as bytes when it exits 0 within the timeout given,
//! or else a one-line reason (git missing, a non-zero exit with the first line git wrote to
//! stderr, a timeout). [`git`] and [`git_text`] are the forms for callers that never fail
//! because of git: stdin closed, [`TIMEOUT`], `None` on any failure. [`cat_file_batch`]
//! reads blobs by object id through one `git cat-file --batch`. Every command is made safe
//! to run in a checkout that belongs to someone else:
//!
//! - stdin closed unless input is given, `GIT_TERMINAL_PROMPT=0`: no prompt can block it;
//! - `--no-optional-locks` and `GIT_OPTIONAL_LOCKS=0`: git takes no lock it does not need,
//!   so `git status` does not refresh and rewrite `.git/index` (taking `index.lock`) while
//!   the user, or their editor, works in the same checkout;
//! - `-c core.fsmonitor=false`: a repository that sets `core.fsmonitor` would otherwise
//!   start git's file system monitor daemon (`true`), which keeps running after the command,
//!   or run the hook program the setting names (a path), a program the repository chooses;
//! - the variables that point git at another repository, index, object store or
//!   configuration (`GIT_DIR`, `GIT_WORK_TREE`, ..., `GIT_CONFIG_PARAMETERS`), or that name
//!   a program for it to run (`GIT_EXTERNAL_DIFF`, `GIT_PAGER`), removed, so the command
//!   reads the repository that contains `dir`, as configured on disk;
//! - killed when the timeout passes;
//! - `LC_ALL=C`: output in a fixed language.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Per git invocation of [`git`] / [`git_text`].
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
    "GIT_EXTERNAL_DIFF",
    "GIT_PAGER",
];

/// Runs `git <args>` in `dir` (module docs) with `input` on stdin (stdin closed when
/// `None`), killed after `timeout`. Its stdout when it exits 0; otherwise `git <first arg>
/// failed: <first non-empty stderr line>`, `git <first arg> timed out after N s`, or
/// `cannot run git: ...`.
pub fn run(
    dir: &Path,
    args: &[&str],
    input: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(dir)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    for v in GIT_ENV {
        cmd.env_remove(v);
    }
    let what = args.first().copied().unwrap_or("git");
    let mut child = cmd.spawn().map_err(|e| format!("cannot run git: {e}"))?;
    // Write and read on threads so neither a large input nor a large output can block the
    // child while we wait. A write error (git exited without reading) is ignored: the exit
    // status decides.
    let writer = match (input, child.stdin.take()) {
        (Some(data), Some(mut stdin)) => Some(thread::spawn(move || {
            let _ = stdin.write_all(&data);
        })),
        _ => None,
    };
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("cannot run git: no output pipe".to_string());
    };
    let out_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < timeout => thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    if let Some(w) = writer {
        let _ = w.join();
    }
    let out = out_reader
        .join()
        .map_err(|_| format!("git {what}: reading its output failed"))?;
    let err = err_reader.join().unwrap_or_default();
    match status {
        Some(s) if s.success() => Ok(out),
        Some(_) => {
            let msg = String::from_utf8_lossy(&err);
            let line = msg.lines().map(str::trim).find(|l| !l.is_empty());
            Err(format!(
                "git {what} failed: {}",
                line.unwrap_or("no message")
            ))
        }
        None => Err(format!(
            "git {what} timed out after {} s",
            timeout.as_secs()
        )),
    }
}

/// Runs `git <args>` in `dir` (module docs); its stdout when it exits 0 within [`TIMEOUT`].
pub fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    run(dir, args, None, TIMEOUT).ok()
}

/// [`git`], with the output decoded as UTF-8 (invalid bytes replaced).
pub fn git_text(dir: &Path, args: &[&str]) -> Option<String> {
    git(dir, args).map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// NUL-separated output (`-z`) split into its non-empty fields, decoded as UTF-8 (invalid
/// bytes replaced).
pub fn nul_fields(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect()
}

/// The contents of the blobs `oids` names, in order, read with one `git cat-file --batch`
/// in `dir` ([`run`] with the object ids on stdin). An entry is `None` when git reports the
/// object `missing` (or ambiguous) or when it is not a blob.
///
/// The output format read is the one `git-cat-file(1)` documents for `--batch`: per object
/// `<oid> SP <type> SP <size> LF <contents> LF`, or `<object> SP missing LF` (also
/// `ambiguous`), with no contents.
pub fn cat_file_batch(
    dir: &Path,
    oids: &[&str],
    timeout: Duration,
) -> Result<Vec<Option<Vec<u8>>>, String> {
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let mut input = Vec::new();
    for oid in oids {
        input.extend_from_slice(oid.as_bytes());
        input.push(b'\n');
    }
    let out = run(dir, &["cat-file", "--batch"], Some(input), timeout)?;
    let mut blobs = Vec::with_capacity(oids.len());
    let mut pos = 0;
    for _ in oids {
        let Some(nl) = out
            .get(pos..)
            .and_then(|r| r.iter().position(|b| *b == b'\n'))
        else {
            blobs.push(None);
            continue;
        };
        let header = String::from_utf8_lossy(&out[pos..pos + nl]).into_owned();
        pos += nl + 1;
        let mut parts = header.split(' ');
        let (_oid, kind, size) = (parts.next(), parts.next(), parts.next());
        let Some(size) = size.and_then(|s| s.parse::<usize>().ok()) else {
            // `<object> missing` / `<object> ambiguous`: no contents follow.
            blobs.push(None);
            continue;
        };
        let end = (pos + size).min(out.len());
        blobs.push((kind == Some("blob")).then(|| out[pos..end].to_vec()));
        pos = end + 1;
    }
    Ok(blobs)
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

    /// A repository with one commit holding `a.txt` (with a NUL byte and no final newline)
    /// and `b.txt`; `None` when git is not installed.
    fn repo() -> Option<tempfile::TempDir> {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        run(d, &["init", "-q"], None, TIMEOUT).ok()?;
        std::fs::write(d.join("a.txt"), b"one\0two").unwrap();
        std::fs::write(d.join("b.txt"), "line\n").unwrap();
        for args in [
            &["add", "."][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "c",
            ],
        ] {
            run(d, args, None, TIMEOUT).unwrap();
        }
        Some(tmp)
    }

    #[test]
    fn a_failure_says_what_git_said() {
        let Some(tmp) = repo() else { return };
        let err = run(
            tmp.path(),
            &["rev-parse", "--verify", "no-such-rev"],
            None,
            TIMEOUT,
        )
        .unwrap_err();
        assert!(err.starts_with("git rev-parse failed: "), "{err}");
        assert!(!err.contains('\n'), "{err}");
    }

    #[test]
    fn input_reaches_stdin_and_cat_file_batch_returns_the_bytes() {
        let Some(tmp) = repo() else { return };
        let d = tmp.path();
        // stdin is fed: `hash-object --stdin` hashes what it reads.
        let oid = run(
            d,
            &["hash-object", "--stdin"],
            Some(b"x\n".to_vec()),
            TIMEOUT,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&oid).trim(),
            "587be6b4c3f93f93c489c0111bba5596147a26cb"
        );
        let tree = run(d, &["ls-tree", "-r", "-z", "HEAD"], None, TIMEOUT).unwrap();
        let oids: Vec<String> = nul_fields(&tree)
            .iter()
            .filter_map(|e| e.split(' ').nth(2).and_then(|s| s.split('\t').next()))
            .map(str::to_string)
            .collect();
        assert_eq!(oids.len(), 2);
        let tree_oid = git_text(d, &["rev-parse", "HEAD^{tree}"]).unwrap();
        let mut ask: Vec<&str> = oids.iter().map(String::as_str).collect();
        // A missing object and a non-blob in the middle do not shift the rest.
        ask.insert(1, "0000000000000000000000000000000000000001");
        ask.insert(2, tree_oid.trim());
        let blobs = cat_file_batch(d, &ask, TIMEOUT).unwrap();
        assert_eq!(
            blobs,
            vec![
                Some(b"one\0two".to_vec()),
                None,
                None,
                Some(b"line\n".to_vec())
            ]
        );
        assert_eq!(
            cat_file_batch(d, &[], TIMEOUT).unwrap(),
            Vec::<Option<Vec<u8>>>::new()
        );
    }
}

//! Background bitbake jobs (T5.4).
//!
//! A [`JobTable`] lives in `mcp::Ctx`. [`JobTable::start`] spawns an allowlisted program
//! through [`BbEnv::command`] with stdout and stderr both redirected to one output file
//! (`<dir>/<pid>-<n>.out`), so the output is on disk as it arrives and nothing has to read
//! a pipe. A tokio task waits for the process ([`Supervised::wait`]) and records how it
//! ended. The table never holds a reference to the handle store: whoever next looks at the
//! table with one ([`JobTable::settle`], called by the `bitbake` tool on every call and by
//! `log` when given `job`; `status` does not call it) stores the output of each finished job
//! under a handle and removes the file.
//!
//! One job runs at a time, of any kind: BitBake allows one client per build directory. A
//! second client finds the lock taken, connects to the running server and retries (8 times,
//! 5 s apart after the fourth: `lib/bb/main.py`), then fails with "Unable to connect to
//! bitbake server, or start one"; live (docs/live-verification.md, "Two clients") a
//! `bitbake-layers` started during a 4 s `bitbake -e` waited and succeeded, but behind a long
//! build it would fail after about 20 s. So sous-chef never starts a second client: a second
//! [`JobTable::start`] while one runs returns [`Started::AlreadyRunning`] with the running
//! job. [`JobTable::start_or_queue`] adds one rule on top: when the running job is
//! read-only and short ([`JobInfo::is_read_only`]: `bitbake -e`, `bitbake-layers`), the new
//! job waits for it, up to [`QUEUE_WAIT`], and then starts; behind a build, a task, a clean
//! or devtool it is refused at once, whatever its own kind, because those hold the lock for
//! minutes to hours.
//!
//! Cancellation ([`JobTable::cancel`]) terminates the job's process group
//! ([`Supervised::terminate`]): SIGTERM, then SIGKILL after
//! [`crate::bitbake::env::KILL_GRACE`], then a wait of up to
//! [`crate::bitbake::env::GROUP_EXIT_WAIT`] until the group is empty; the job is recorded
//! as cancelled only after that, so a job that is no longer running has no process left in
//! its group (unless one outlived that wait). Unverified: whether a BitBake server started
//! by the job detaches from the group and survives that.
//!
//! When the server exits (the client closes stdin, `serve` returns and `main` drops the
//! tokio runtime), the task waiting for a running job is dropped with the runtime, and
//! dropping its [`Supervised`] sends SIGKILL to the job's process group (and
//! `kill_on_drop` to the leader); nothing waits for the group. A server killed by a signal
//! runs no destructor: its job's group, a group of its own, keeps running. Either way the
//! output file stays behind.
//!
//! Output files of a server that is gone are removed when the next server opens its
//! context (`mcp::Ctx::open` calls [`JobTable::sweep_in_background`] on `<cache dir>/jobs`,
//! on a thread of its own so start-up does not wait), or, when that thread could not be
//! started or a job writes to another directory, by the first [`JobTable::start`] in that
//! directory ([`remove_stale_outputs`]): a `<pid>-<n>.out` whose
//! pid is not a live process and which was last modified more than [`STALE_AFTER`] ago. A
//! file whose pid is alive is never removed (a reused pid keeps a stale file until that
//! process exits); the age guards a server in another pid namespace sharing the cache
//! directory, whose pid is not visible here.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::Notify;

use crate::bitbake::env::{BbEnv, Finish, ProcessGroup, Supervised};
use crate::error::{Error, Result};
use crate::handles::{Handle, HandleKind, HandleStore};

/// The state of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Running => "running",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }
}

/// What to run. The caller has validated every user-supplied name in `args`
/// ([`crate::bitbake::validate_name`]); [`BbEnv::command`] checks the program and the
/// argument syntax again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    /// The tool operation (`build`, `task`, `env`, `clean`, `devtool`, `layers`).
    pub op: String,
    /// The recipe or image, if any.
    pub target: Option<String>,
    /// An allowlisted program (`bitbake`, `bitbake-layers`, `devtool`).
    pub program: String,
    pub args: Vec<String>,
    /// The kind of handle the output is stored under.
    pub kind: HandleKind,
}

impl JobSpec {
    /// `program args...`, as shown to the agent and used as the handle's command.
    pub fn command_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A snapshot of one job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobInfo {
    /// `job:<n>`, unique within this server process.
    pub id: String,
    pub op: String,
    pub target: Option<String>,
    /// `program args...`.
    pub command: String,
    pub state: JobState,
    /// Seconds since the Unix epoch.
    pub started: u64,
    pub ended: Option<u64>,
    /// Seconds from start to end, or to now while running.
    pub elapsed_secs: u64,
    /// The exit status, when the process exited on its own.
    pub exit_code: Option<i32>,
    pub kind: HandleKind,
    /// The output, once [`JobTable::settle`] has stored it.
    pub handle: Option<Handle>,
    /// Why the output could not be stored, if it could not.
    pub handle_error: Option<String>,
    /// Where the output is written while the job runs (removed once stored).
    pub output: PathBuf,
}

impl JobInfo {
    /// Whether the job only reads (`bitbake -e`, operation `env`; `bitbake-layers show-*`,
    /// operation `layers`): seconds to a minute on a real build directory, so a new job may
    /// wait for it ([`JobTable::start_or_queue`]).
    pub fn is_read_only(&self) -> bool {
        matches!(self.op.as_str(), "env" | "layers")
    }
}

/// How long [`JobTable::start_or_queue`] waits for a running read-only job before giving
/// up (a `bitbake -e` takes about 15 s on a real workspace, a `bitbake-layers` a few).
pub const QUEUE_WAIT: Duration = Duration::from_secs(30);

/// Result of [`JobTable::start`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    Started(JobInfo),
    /// Another job is running; nothing was started.
    AlreadyRunning(JobInfo),
}

#[derive(Debug)]
struct Entry {
    info: JobInfo,
    started: Instant,
    ended: Option<Instant>,
    cancel: Arc<Notify>,
    group: ProcessGroup,
}

impl Entry {
    fn snapshot(&self) -> JobInfo {
        let mut info = self.info.clone();
        let end = self.ended.unwrap_or_else(Instant::now);
        info.elapsed_secs = end.saturating_duration_since(self.started).as_secs();
        info
    }
}

#[derive(Debug, Default)]
struct Inner {
    next: u64,
    jobs: Vec<Entry>,
    /// Output directories [`remove_stale_outputs`] has swept for this table.
    swept: Vec<PathBuf>,
}

/// Output files older than this whose pid is not alive are removed (module docs). A job's
/// file is written as its output arrives, so a running job's file is this old only after
/// that long without output.
pub const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// The pid in a job output file name, `<pid>-<n>.out` (both decimal), else `None`.
pub fn output_pid(name: &str) -> Option<u32> {
    let (pid, n) = name.strip_suffix(".out")?.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (digits(pid) && digits(n))
        .then(|| pid.parse().ok())
        .flatten()
}

/// Removes from `dir` the job output files (`<pid>-<n>.out`) whose pid is not a live
/// process ([`crate::bitbake::env::process_alive`]) and that were last modified more than
/// `older_than` ago. Never a file of a live pid, of this process, or any other name. Returns
/// the files removed; a missing directory is no error.
pub fn remove_stale_outputs(dir: &Path, older_than: Duration) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return removed;
    };
    let me = std::process::id();
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(output_pid) else {
            continue;
        };
        if pid == me || crate::bitbake::env::process_alive(pid) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > older_than);
        if meta.is_file() && old && fs::remove_file(entry.path()).is_ok() {
            removed.push(entry.path());
        }
    }
    removed.sort();
    removed
}

/// Table of background jobs, held in `mcp::Ctx`. Methods take `&self`; clones share the
/// same table.
#[derive(Debug, Default, Clone)]
pub struct JobTable {
    inner: Arc<Mutex<Inner>>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Accepts `job:3` or `3`.
fn normalize_id(id: &str) -> String {
    let id = id.trim();
    if id.starts_with("job:") {
        id.to_string()
    } else {
        format!("job:{id}")
    }
}

impl JobTable {
    pub fn new() -> JobTable {
        JobTable::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Removes the stale output files in `dir` ([`remove_stale_outputs`]) on a thread of its
    /// own, so the caller (`mcp::Ctx::open`, at server start) does not wait for it, and
    /// records `dir` as swept so [`JobTable::start`] does not sweep it again. Returns the
    /// thread's handle (the files it removed), or `None` when no thread could be started;
    /// then the first job started in `dir` sweeps it.
    pub fn sweep_in_background(&self, dir: &Path) -> Option<std::thread::JoinHandle<Vec<PathBuf>>> {
        let mut inner = self.lock();
        if inner.swept.iter().any(|d| d == dir) {
            return None;
        }
        let owned = dir.to_path_buf();
        let handle = std::thread::Builder::new()
            .name("sous-chef-stale-outputs".into())
            .spawn(move || remove_stale_outputs(&owned, STALE_AFTER))
            .ok()?;
        inner.swept.push(dir.to_path_buf());
        Some(handle)
    }

    /// Starts `spec` in `env`, writing its output under `dir`. Must be called inside a
    /// tokio runtime. Returns the running job instead when one is running.
    pub fn start(&self, env: &BbEnv, spec: JobSpec, dir: &Path) -> Result<Started> {
        let mut inner = self.lock();
        if let Some(running) = inner
            .jobs
            .iter()
            .find(|e| e.info.state == JobState::Running)
        {
            return Ok(Started::AlreadyRunning(running.snapshot()));
        }
        let args: Vec<&str> = spec.args.iter().map(String::as_str).collect();
        let mut cmd = env.command(&spec.program, &args)?;
        fs::create_dir_all(dir)?;
        if !inner.swept.iter().any(|d| d == dir) {
            remove_stale_outputs(dir, STALE_AFTER);
            inner.swept.push(dir.to_path_buf());
        }
        let n = inner.next + 1;
        let output = dir.join(format!("{}-{n}.out", std::process::id()));
        let file = File::create(&output)?;
        cmd.stdout(Stdio::from(file.try_clone()?))
            .stderr(Stdio::from(file));
        let mut proc = match Supervised::spawn(cmd) {
            Ok(p) => p,
            Err(e) => {
                let _ = fs::remove_file(&output);
                return Err(e);
            }
        };
        inner.next = n;

        let id = format!("job:{n}");
        let cancel = Arc::new(Notify::new());
        let entry = Entry {
            info: JobInfo {
                id: id.clone(),
                op: spec.op.clone(),
                target: spec.target.clone(),
                command: spec.command_line(),
                state: JobState::Running,
                started: now_secs(),
                ended: None,
                elapsed_secs: 0,
                exit_code: None,
                kind: spec.kind,
                handle: None,
                handle_error: None,
                output,
            },
            started: Instant::now(),
            ended: None,
            cancel: Arc::clone(&cancel),
            group: proc.group(),
        };
        let info = entry.snapshot();
        inner.jobs.push(entry);
        drop(inner);

        let table = self.clone();
        tokio::spawn(async move {
            let finish = proc
                .wait(None, async move { cancel.notified().await })
                .await;
            table.finished(&id, finish);
        });
        Ok(Started::Started(info))
    }

    /// Records how a job ended.
    fn finished(&self, id: &str, finish: Result<Finish>) {
        let mut inner = self.lock();
        let Some(e) = inner.jobs.iter_mut().find(|e| e.info.id == id) else {
            return;
        };
        let (state, code) = match finish {
            Ok(Finish::Exited(status)) if status.success() => (JobState::Succeeded, status.code()),
            Ok(Finish::Exited(status)) => (JobState::Failed, status.code()),
            Ok(Finish::Cancelled) => (JobState::Cancelled, None),
            Ok(Finish::TimedOut) | Err(_) => (JobState::Failed, None),
        };
        e.info.state = state;
        e.info.exit_code = code;
        e.info.ended = Some(now_secs());
        e.ended = Some(Instant::now());
    }

    /// One job by id (`job:3` or `3`).
    pub fn get(&self, id: &str) -> Option<JobInfo> {
        let id = normalize_id(id);
        self.lock()
            .jobs
            .iter()
            .find(|e| e.info.id == id)
            .map(Entry::snapshot)
    }

    /// The running job, if any.
    pub fn running(&self) -> Option<JobInfo> {
        self.lock()
            .jobs
            .iter()
            .find(|e| e.info.state == JobState::Running)
            .map(Entry::snapshot)
    }

    /// The most recently started job.
    pub fn last(&self) -> Option<JobInfo> {
        self.lock().jobs.last().map(Entry::snapshot)
    }

    /// Every job, oldest first.
    pub fn list(&self) -> Vec<JobInfo> {
        self.lock().jobs.iter().map(Entry::snapshot).collect()
    }

    /// Asks a running job to stop: its process group is terminated (module docs), after
    /// which the job is recorded as cancelled. Returns the job as it is now, still running;
    /// see [`JobTable::wait`] for the end.
    pub fn cancel(&self, id: &str) -> Result<JobInfo> {
        let id = normalize_id(id);
        let inner = self.lock();
        let e = inner
            .jobs
            .iter()
            .find(|e| e.info.id == id)
            .ok_or_else(|| Error::NotFound {
                what: format!("{id}; `bitbake op=status` shows the last job"),
            })?;
        if e.info.state != JobState::Running {
            return Err(Error::Invalid {
                msg: format!("{id} is not running (it {})", e.info.state.as_str()),
            });
        }
        e.cancel.notify_one();
        Ok(e.snapshot())
    }

    /// Waits until job `id` is no longer running, at most `timeout`. Returns the job.
    pub async fn wait(&self, id: &str, timeout: Duration) -> Option<JobInfo> {
        self.wait_with(id, timeout, |_| {}).await
    }

    /// As [`JobTable::wait`], calling `tick` with the job as it is now about every 20 ms
    /// while it runs (the caller rate-limits what it does with it: progress notifications).
    pub async fn wait_with(
        &self,
        id: &str,
        timeout: Duration,
        mut tick: impl FnMut(&JobInfo),
    ) -> Option<JobInfo> {
        let deadline = Instant::now() + timeout;
        loop {
            let info = self.get(id)?;
            if info.state != JobState::Running || Instant::now() >= deadline {
                return Some(info);
            }
            tick(&info);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// [`JobTable::start`], except that while a read-only job runs
    /// ([`JobInfo::is_read_only`]) it waits for that job to end, up to `max_wait` in all
    /// (normally [`QUEUE_WAIT`]), and then starts `spec`; `tick` sees the job waited for, as
    /// in [`JobTable::wait_with`]. A running job that is not read-only, or a read-only job
    /// still running after `max_wait`, gives [`Started::AlreadyRunning`] at once (module
    /// docs). Never two jobs at a time: the start itself is [`JobTable::start`].
    pub async fn start_or_queue(
        &self,
        env: &BbEnv,
        spec: JobSpec,
        dir: &Path,
        max_wait: Duration,
        mut tick: impl FnMut(&JobInfo),
    ) -> Result<Started> {
        let deadline = Instant::now() + max_wait;
        loop {
            match self.start(env, spec.clone(), dir)? {
                Started::Started(job) => return Ok(Started::Started(job)),
                Started::AlreadyRunning(running) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if !running.is_read_only() || left.is_zero() {
                        return Ok(Started::AlreadyRunning(running));
                    }
                    let after = self.wait_with(&running.id, left, &mut tick).await;
                    if after.is_some_and(|j| j.state == JobState::Running) {
                        return Ok(Started::AlreadyRunning(
                            self.get(&running.id).unwrap_or(running),
                        ));
                    }
                }
            }
        }
    }

    /// Stores the output of every finished job that has no handle yet, and removes its
    /// output file. A job whose output cannot be stored keeps its file and records why.
    pub fn settle(&self, handles: &HandleStore) {
        let mut inner = self.lock();
        for e in inner.jobs.iter_mut() {
            if e.info.state == JobState::Running
                || e.info.handle.is_some()
                || e.info.handle_error.is_some()
            {
                continue;
            }
            let stored = fs::read(&e.info.output)
                .map_err(Error::from)
                .and_then(|bytes| handles.put(e.info.kind, &e.info.command, &bytes));
            match stored {
                Ok(h) => {
                    e.info.handle = Some(h);
                    let _ = fs::remove_file(&e.info.output);
                }
                Err(err) => e.info.handle_error = Some(err.to_string()),
            }
        }
    }

    /// The process group of a running job (for tests and diagnostics).
    pub fn process_group(&self, id: &str) -> Option<ProcessGroup> {
        let id = normalize_id(id);
        self.lock()
            .jobs
            .iter()
            .find(|e| e.info.id == id && e.info.state == JobState::Running)
            .map(|e| e.group)
    }
}

/// How much of the end of an output file [`tail`] reads.
const TAIL_BYTES: u64 = 16 * 1024;

/// The last `n` non-empty lines of the output file at `path` (read from its last 16 KB).
pub fn tail(path: &Path, n: usize) -> Vec<String> {
    let Ok(mut f) = File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_BYTES);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    tail_of(&String::from_utf8_lossy(&buf), n)
}

/// The last `n` non-empty lines of `text`.
pub fn tail_of(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_tails() {
        assert_eq!(normalize_id("3"), "job:3");
        assert_eq!(normalize_id(" job:3 "), "job:3");
        assert_eq!(tail_of("a\n\nb\r\nc\n", 2), ["b", "c"]);
        assert_eq!(tail_of("a\n", 5), ["a"]);
        assert!(tail(Path::new("/nonexistent/x"), 3).is_empty());
        let t = JobTable::new();
        assert!(t.running().is_none() && t.last().is_none() && t.get("1").is_none());
        assert!(matches!(t.cancel("1"), Err(Error::NotFound { .. })));
    }

    #[test]
    fn output_file_names() {
        assert_eq!(output_pid("1234-5.out"), Some(1234));
        for bad in [
            "1234.out",
            "-5.out",
            "1234-.out",
            "12a4-5.out",
            "1234-5.txt",
            "x",
        ] {
            assert_eq!(output_pid(bad), None, "{bad}");
        }
    }

    /// The pid of a child that has exited and been reaped: not a live process (unless the
    /// system reuses it within the test, which a 4-million pid space makes unlikely).
    #[cfg(unix)]
    fn dead_pid() -> u32 {
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        assert!(!crate::bitbake::env::process_alive(pid));
        pid
    }

    #[cfg(unix)]
    #[test]
    fn stale_outputs_of_dead_servers_are_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let old = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        let put = |name: &str, age_old: bool| {
            let p = d.join(name);
            let f = File::create(&p).unwrap();
            if age_old {
                f.set_modified(old).unwrap();
            }
            p
        };
        let dead = dead_pid();
        let me = std::process::id();
        let parent = std::os::unix::process::parent_id();
        let stale = put(&format!("{dead}-1.out"), true);
        let fresh = put(&format!("{dead}-2.out"), false);
        let mine = put(&format!("{me}-1.out"), true);
        let live = put(&format!("{parent}-1.out"), true);
        let other = put("notes-1.txt", true);
        let removed = remove_stale_outputs(d, STALE_AFTER);
        assert_eq!(removed, std::slice::from_ref(&stale));
        assert!(!stale.exists());
        for kept in [&fresh, &mine, &live, &other] {
            assert!(kept.exists(), "{}", kept.display());
        }
        assert!(remove_stale_outputs(&d.join("missing"), STALE_AFTER).is_empty());
    }

    #[test]
    fn a_background_sweep_runs_once_per_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let stale = d.join(format!("{}-1.out", dead_pid()));
        File::create(&stale)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60))
            .unwrap();
        let table = JobTable::new();
        let removed = table.sweep_in_background(d).unwrap().join().unwrap();
        assert_eq!(removed, std::slice::from_ref(&stale));
        // Recorded as swept: neither a second sweep nor the first job sweeps it again.
        assert!(table.sweep_in_background(d).is_none());
        assert!(table.lock().swept.iter().any(|p| p == d));
    }
}

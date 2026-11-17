//! Background bitbake jobs (T5.4).
//!
//! A [`JobTable`] lives in `mcp::Ctx`. [`JobTable::start`] spawns an allowlisted program
//! through [`BbEnv::command`] with stdout and stderr both redirected to one output file
//! (`<dir>/<pid>-<n>.out`), so the output is on disk as it arrives and nothing has to read
//! a pipe. A tokio task waits for the process ([`Supervised::wait`]) and records how it
//! ended. The table never holds a reference to the handle store: whoever next looks at the
//! table with one ([`JobTable::settle`], called by the `bitbake`, `log` and `status`
//! tools) stores the output of each finished job under a handle and removes the file.
//!
//! One job runs at a time, of any kind: BitBake takes a lock on the build directory, so a
//! second `bitbake -e` or `bitbake-layers` while a build runs would wait for it or fail.
//! A second [`JobTable::start`] while one runs returns [`Started::AlreadyRunning`] with the
//! running job.
//!
//! Cancellation ([`JobTable::cancel`]) terminates the job's process group: SIGTERM, then
//! SIGKILL after [`crate::bitbake::env::KILL_GRACE`]. Unverified: whether a BitBake server
//! started by the job detaches from the group and survives that.

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

    /// Asks a running job to stop (its process group is terminated). Returns the job as it
    /// is now; see [`JobTable::wait`] for the end.
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
        let deadline = Instant::now() + timeout;
        loop {
            let info = self.get(id)?;
            if info.state != JobState::Running || Instant::now() >= deadline {
                return Some(info);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
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
}

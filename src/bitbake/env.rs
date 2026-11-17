//! Runs a command inside the initialised build environment.
//!
//! Two modes:
//!
//! - **init script**: every command is
//!   `bash -c 'source "$1" "$2" >/dev/null && shift 2 && exec "$@"' -- <init> <build_dir> <program> <args...>`,
//!   so the script, the build directory, the program and every argument travel as argv and
//!   are never interpolated into shell text;
//! - **initialised**: the server was started inside an environment that is already set up
//!   (`BUILDDIR` or `BBPATH` set, `bitbake` on `PATH`); the program is executed directly.
//!
//! Only allowlisted programs run, and every argument is validated. Each command runs in its
//! own process group, so a timeout or cancellation ([`Supervised`]) kills the whole tree
//! (bash, bitbake and whatever they started), not just the direct child.

use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::workspace::Workspace;

/// Programs [`BbEnv::command`] runs. Anything else is refused.
pub const ALLOWED_PROGRAMS: [&str; 5] = [
    "bitbake",
    "bitbake-layers",
    "bitbake-getvar",
    "devtool",
    "oe-pkgdata-util",
];

/// The init script searched for next to the layers when `init_script` is not configured.
pub const INIT_SCRIPT_NAME: &str = "oe-init-build-env";

/// The wrapper run by `bash -c` in init-script mode. Positional parameters only.
pub const WRAPPER: &str = r#"source "$1" "$2" >/dev/null && shift 2 && exec "$@""#;

/// How long a terminated process group gets between SIGTERM and SIGKILL.
pub const KILL_GRACE: Duration = Duration::from_secs(2);

/// A usable BitBake environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BbEnv {
    pub build_dir: PathBuf,
    /// `None` when the process environment is already initialised.
    pub init_script: Option<PathBuf>,
}

/// The parts of the process environment detection looks at. Tests build one directly
/// instead of changing the real environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostEnv {
    /// `BUILDDIR`
    pub builddir: Option<PathBuf>,
    /// `BBPATH`
    pub bbpath: Option<OsString>,
    /// `PATH`
    pub path: Option<OsString>,
}

impl HostEnv {
    /// Reads `BUILDDIR`, `BBPATH` and `PATH` from this process. Empty values count as unset.
    pub fn from_process() -> HostEnv {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        HostEnv {
            builddir: var("BUILDDIR").map(PathBuf::from),
            bbpath: var("BBPATH"),
            path: var("PATH"),
        }
    }
}

/// What `status` reports about the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvState {
    pub env: Option<BbEnv>,
    /// Why no environment was found (one line), when `env` is `None`.
    pub problem: Option<String>,
}

/// Detects the environment from the configuration, the workspace and this process.
pub fn environment_state(cfg: &Config, ws: &Workspace) -> EnvState {
    match BbEnv::probe(cfg, ws, &HostEnv::from_process()) {
        Ok(env) => EnvState {
            env: Some(env),
            problem: None,
        },
        Err(problem) => EnvState {
            env: None,
            problem: Some(problem),
        },
    }
}

impl BbEnv {
    /// Finds a build directory and init script, or an already-initialised environment.
    pub fn detect(cfg: &Config, ws: &Workspace) -> Option<BbEnv> {
        BbEnv::detect_in(cfg, ws, &HostEnv::from_process())
    }

    /// [`BbEnv::detect`] against an explicit process environment.
    pub fn detect_in(cfg: &Config, ws: &Workspace, host: &HostEnv) -> Option<BbEnv> {
        BbEnv::probe(cfg, ws, host).ok()
    }

    /// Detection with the reason it failed. In order:
    ///
    /// 1. `[bitbake] enabled = false`: none.
    /// 2. `init_script` configured: it must be a file, and a build directory must exist
    ///    (the workspace's, else `build_dir` from the configuration).
    /// 3. Already initialised: `BUILDDIR` or `BBPATH` set and an executable `bitbake` on
    ///    `PATH`. The build directory is `BUILDDIR` if it is a directory, else the
    ///    workspace's.
    /// 4. A build directory and an `oe-init-build-env` in a layer root, a layer root's
    ///    parent (poky's layout: `poky/meta` next to `poky/oe-init-build-env`) or the build
    ///    directory's parent, searched in BBLAYERS order.
    pub fn probe(
        cfg: &Config,
        ws: &Workspace,
        host: &HostEnv,
    ) -> std::result::Result<BbEnv, String> {
        if !cfg.bitbake.enabled {
            return Err("disabled in sous-chef.toml ([bitbake] enabled = false)".into());
        }
        let build_dir = ws
            .build_dir
            .clone()
            .or_else(|| cfg.build_dir.clone())
            .filter(|d| d.is_dir())
            .map(|d| d.canonicalize().unwrap_or(d));

        if let Some(script) = &cfg.init_script {
            if !script.is_file() {
                return Err(format!(
                    "init_script {} is not a file; fix init_script in sous-chef.toml",
                    script.display()
                ));
            }
            let Some(build_dir) = build_dir else {
                return Err(
                    "init_script is set but there is no build directory; set build_dir or --build-dir"
                        .into(),
                );
            };
            return Ok(BbEnv {
                build_dir,
                init_script: Some(absolute(script)),
            });
        }

        let initialised = host.builddir.is_some() || host.bbpath.is_some();
        if initialised && find_on_path("bitbake", host.path.as_deref()).is_some() {
            let dir = host
                .builddir
                .clone()
                .filter(|d| d.is_dir())
                .map(|d| d.canonicalize().unwrap_or(d))
                .or_else(|| build_dir.clone());
            if let Some(build_dir) = dir {
                return Ok(BbEnv {
                    build_dir,
                    init_script: None,
                });
            }
        }

        let Some(build_dir) = build_dir else {
            return Err(
                "no build directory and no initialised environment; set build_dir or start sous-chef after sourcing oe-init-build-env"
                    .into(),
            );
        };
        let mut candidates: Vec<PathBuf> = Vec::new();
        for layer in &ws.layers {
            candidates.push(layer.root.join(INIT_SCRIPT_NAME));
            if let Some(parent) = layer.root.parent() {
                candidates.push(parent.join(INIT_SCRIPT_NAME));
            }
        }
        if let Some(parent) = build_dir.parent() {
            candidates.push(parent.join(INIT_SCRIPT_NAME));
        }
        match candidates.into_iter().find(|c| c.is_file()) {
            Some(script) => Ok(BbEnv {
                build_dir,
                init_script: Some(script),
            }),
            None => Err(format!(
                "no {INIT_SCRIPT_NAME} found next to the layers; set init_script in sous-chef.toml"
            )),
        }
    }

    /// `init-script` or `initialised`.
    pub fn mode(&self) -> &'static str {
        if self.init_script.is_some() {
            "init-script"
        } else {
            "initialised"
        }
    }

    /// A command running an allowlisted `program` with `args` in the environment. Arguments
    /// are passed as argv, never interpolated into a shell script.
    ///
    /// Each argument must pass [`validate_arg`]; callers validate user-supplied recipe,
    /// task and image names with [`validate_name`] first, so that a name can never be
    /// taken for an option.
    ///
    /// The command runs in the build directory, with stdin closed, in a new process group
    /// (kill it through [`Supervised`]). Stdout and stderr are inherited unless the caller
    /// sets them.
    pub fn command(&self, program: &str, args: &[&str]) -> Result<Command> {
        if !ALLOWED_PROGRAMS.contains(&program) {
            return Err(Error::Invalid {
                msg: format!(
                    "program {program:?} is not allowed; allowed: {}",
                    ALLOWED_PROGRAMS.join(", ")
                ),
            });
        }
        for a in args {
            validate_arg(a)?;
        }
        let mut cmd = match &self.init_script {
            Some(script) => {
                let mut c = Command::new("bash");
                c.arg("-c")
                    .arg(WRAPPER)
                    .arg("--")
                    .arg(script)
                    .arg(&self.build_dir)
                    .arg(program)
                    .args(args);
                c
            }
            None => {
                let mut c = Command::new(program);
                c.args(args);
                c
            }
        };
        cmd.current_dir(&self.build_dir)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        Ok(cmd)
    }

    /// Runs `program args` to completion with stdout and stderr captured. On `timeout` the
    /// process group is terminated and the result is [`Finish::TimedOut`] with whatever
    /// output arrived.
    pub async fn output(
        &self,
        program: &str,
        args: &[&str],
        timeout: Option<Duration>,
    ) -> Result<CommandOutput> {
        let mut cmd = self.command(program, args)?;
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut proc = Supervised::spawn(cmd)?;
        let out = collect(proc.child_mut().stdout.take());
        let err = collect(proc.child_mut().stderr.take());
        let finish = proc.wait(timeout, std::future::pending()).await?;
        Ok(CommandOutput {
            finish,
            stdout: out.finish().await,
            stderr: err.finish().await,
        })
    }
}

/// Validates a recipe, task, image or package name: only `A-Z a-z 0-9 _ . + - /`, not
/// empty and no leading `-`.
pub fn validate_name(s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-' | b'/'));
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid {
            msg: format!(
                "invalid name {s:?}: use only letters, digits and _ . + - /, not starting with -"
            ),
        })
    }
}

/// Validates one argument of [`BbEnv::command`]: a name ([`validate_name`]) or a plain
/// option (`-c`, `--force`: one or two dashes, a letter, then letters, digits and `-`;
/// no `=` value).
pub fn validate_arg(s: &str) -> Result<()> {
    let opt = s
        .strip_prefix("--")
        .or_else(|| s.strip_prefix('-'))
        .is_some_and(|rest| {
            rest.starts_with(|c: char| c.is_ascii_alphabetic())
                && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    if opt {
        return Ok(());
    }
    validate_name(s).map_err(|_| Error::Invalid {
        msg: format!(
            "invalid argument {s:?}: use only letters, digits and _ . + - /, or a plain option such as -c"
        ),
    })
}

/// The executable `name` in the directories of `path`, if any.
fn find_on_path(name: &str, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

fn absolute(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// How a supervised process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    /// It exited on its own.
    Exited(ExitStatus),
    /// The timeout passed; the process group was killed.
    TimedOut,
    /// The cancel future completed; the process group was killed.
    Cancelled,
}

impl Finish {
    /// True for an exit with status 0.
    pub fn success(&self) -> bool {
        matches!(self, Finish::Exited(s) if s.success())
    }
}

/// Result of [`BbEnv::output`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub finish: Finish,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// The process group of a spawned command. Cheap to copy; [`ProcessGroup::kill`] can be
/// called from any task (cancellation from another tool call).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessGroup {
    pgid: Option<u32>,
}

impl ProcessGroup {
    pub fn id(&self) -> Option<u32> {
        self.pgid
    }

    /// Sends SIGTERM to every process in the group.
    pub fn terminate(&self) {
        if let Some(p) = self.pgid {
            sys::signal_group(p, sys::SIGTERM);
        }
    }

    /// Sends SIGKILL to every process in the group.
    pub fn kill(&self) {
        if let Some(p) = self.pgid {
            sys::signal_group(p, sys::SIGKILL);
        }
    }
}

/// A spawned command that owns its process group. Waiting with a timeout or a cancel
/// future terminates the group (SIGTERM, then SIGKILL after [`KILL_GRACE`]). Dropping it
/// before the process was reaped kills the group.
#[derive(Debug)]
pub struct Supervised {
    child: Child,
    group: ProcessGroup,
    reaped: bool,
}

impl Supervised {
    /// Spawns `cmd` as the leader of a new process group. Use with a command from
    /// [`BbEnv::command`] (which already asks for one).
    pub fn spawn(mut cmd: Command) -> Result<Supervised> {
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.kill_on_drop(true);
        let child = cmd.spawn()?;
        // With process_group(0) the group id is the child's pid.
        let group = ProcessGroup { pgid: child.id() };
        Ok(Supervised {
            child,
            group,
            reaped: false,
        })
    }

    /// The child, e.g. to take its stdout.
    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub fn group(&self) -> ProcessGroup {
        self.group
    }

    /// Waits for the process to exit, at most `timeout`, or until `cancel` completes. On
    /// timeout or cancellation the whole group is terminated before this returns.
    pub async fn wait(
        &mut self,
        timeout: Option<Duration>,
        cancel: impl Future<Output = ()>,
    ) -> Result<Finish> {
        let sleep = async {
            match timeout {
                Some(t) => tokio::time::sleep(t).await,
                None => std::future::pending().await,
            }
        };
        let outcome = tokio::select! {
            status = self.child.wait() => Ok(status),
            () = sleep => Err(Finish::TimedOut),
            () = cancel => Err(Finish::Cancelled),
        };
        match outcome {
            Ok(status) => {
                self.reaped = true;
                Ok(Finish::Exited(status?))
            }
            Err(finish) => {
                self.terminate().await?;
                Ok(finish)
            }
        }
    }

    /// SIGTERM to the group, up to [`KILL_GRACE`] for the leader to exit, then SIGKILL to
    /// the group (for anything that ignored SIGTERM) and reap the leader.
    pub async fn terminate(&mut self) -> Result<()> {
        self.group.terminate();
        let _ = tokio::time::timeout(KILL_GRACE, self.child.wait()).await;
        self.group.kill();
        self.child.wait().await?;
        self.reaped = true;
        Ok(())
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        if !self.reaped {
            self.group.kill();
        }
    }
}

/// Reads a pipe to the end in a background task.
struct Collector {
    buf: Arc<Mutex<Vec<u8>>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

fn collect<R: AsyncRead + Unpin + Send + 'static>(pipe: Option<R>) -> Collector {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let task = pipe.map(|mut pipe| {
        let buf = Arc::clone(&buf);
        tokio::spawn(async move {
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut b) = buf.lock() {
                            b.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            }
        })
    });
    Collector { buf, task }
}

impl Collector {
    /// What was read. Waits briefly for the end of the pipe: a process that escaped the
    /// group (a daemon) may hold it open forever.
    async fn finish(mut self) -> Vec<u8> {
        if let Some(task) = self.task.take() {
            let abort = task.abort_handle();
            if tokio::time::timeout(KILL_GRACE, task).await.is_err() {
                abort.abort();
            }
        }
        self.buf.lock().map(|b| b.clone()).unwrap_or_default()
    }
}

#[cfg(unix)]
mod sys {
    pub const SIGTERM: i32 = 15;
    pub const SIGKILL: i32 = 9;

    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    /// `kill(-pgid, sig)`. Refuses group ids that are not a real child's (0 and 1 would
    /// signal our own group or every process).
    pub fn signal_group(pgid: u32, sig: i32) {
        let Ok(p) = i32::try_from(pgid) else { return };
        if p <= 1 {
            return;
        }
        // SAFETY: kill(2) takes plain integers and has no memory-safety preconditions; a
        // stale group id only yields ESRCH, which is ignored.
        unsafe {
            kill(-p, sig);
        }
    }
}

#[cfg(not(unix))]
mod sys {
    pub const SIGTERM: i32 = 15;
    pub const SIGKILL: i32 = 9;
    pub fn signal_group(_pgid: u32, _sig: i32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in [
            "core-image-minimal",
            "virtual/kernel",
            "gtk+3",
            "python3-foo_1.0",
            "a.b",
        ] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "-c", "a;b", "$(id)", "`id`", "a b", "a|b", "a&b", "a>b", "a\nb", "a,b", "a:b",
            "a'b", "a\"b", "a=b", "~", "*",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn args() {
        for ok in ["-c", "-e", "-k", "--force", "-f", "compile", "show-layers"] {
            assert!(validate_arg(ok).is_ok(), "{ok}");
        }
        for bad in ["-", "--", "-1", "--x=1", "-c;id", "--$(id)", "-a b"] {
            assert!(validate_arg(bad).is_err(), "{bad:?}");
        }
    }
}

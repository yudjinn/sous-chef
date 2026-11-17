//! Runs a command inside the initialised build environment.
//!
//! Four modes:
//!
//! - **init script**: every command is
//!   `bash -c '<save argv>; source "$1" "$2" >/dev/null && exec <saved argv from 3>' -- <init> <build_dir> <program> <args...>`
//!   ([`WRAPPER`]),
//!   so the script, the build directory, the program and every argument travel as argv and
//!   are never interpolated into shell text;
//! - **bitbake-setup**: the init script is a `bitbake-setup` `init-build-env` (it takes no
//!   arguments); [`SETUP_WRAPPER`] instead of [`WRAPPER`], argv as above;
//! - **initialised**: the server was started inside an environment that is already set up
//!   (`BUILDDIR` or `BBPATH` set, `bitbake` on `PATH`); the program is executed directly;
//! - **kas**: the workspace is a kas project and `kas` is on `PATH`; every command is
//!   `kas shell --keep-config-unchanged <a.yaml:b.yaml> -c '<command>'`, run in kas's work
//!   directory. kas takes the command as one string and hands it to the user's `$SHELL`
//!   (`$SHELL -c`), so this is the one mode that builds shell text: the string is made
//!   only of the allowlisted program and arguments that passed [`validate_arg`] (letters,
//!   digits, `_ . + - /`), each in single quotes, which every POSIX shell, bash and zsh take
//!   literally ([`kas_command_string`]).
//!
//! Only allowlisted programs run, and every argument is validated. Each command runs in its
//! own process group, so a timeout or cancellation ([`Supervised`]) signals the whole tree
//! (bash, bitbake and whatever they started), not just the direct child, and waits (up to
//! [`GROUP_EXIT_WAIT`]) until no process is left in the group. A process that left the
//! group (`setsid`, a daemon) is not reached.

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

/// The wrapper run by `bash -c` in init-script mode. Positional parameters only. The argv is
/// saved in an array before the script is sourced: bash keeps the positional parameters a
/// sourced file changes (`set --` at the end of bitbake-setup's `init-build-env`, also when
/// another script sources it), so `shift 2 && exec "$@"` would run nothing (live
/// verification B7).
pub const WRAPPER: &str =
    r#"__sous_chef_argv=("$@"); source "$1" "$2" >/dev/null && exec "${__sous_chef_argv[@]:2}""#;

/// The script `bitbake-setup` writes into the build directory of each setup.
pub const SETUP_SCRIPT_NAME: &str = "init-build-env";

/// The wrapper run by `bash -c` for a `bitbake-setup` build. Its `init-build-env` takes no
/// arguments and runs `set <builddir>`, sources `oe-init-build-env`, then `set --`; bash
/// keeps positional parameters a sourced file changes, so [`WRAPPER`] would lose the
/// program. The argv is saved in an array first; still positional parameters only.
pub const SETUP_WRAPPER: &str = r#"__sous_chef_argv=("$@"); set --; source "${__sous_chef_argv[0]}" >/dev/null && exec "${__sous_chef_argv[@]:1}""#;

/// The kas subcommand and option every kas-mode command uses: `kas shell
/// --keep-config-unchanged`, which skips kas's checkout and configuration steps
/// (`setup_dir`, `finish_setup_repos`, `repos_checkout`, `repos_apply_patches`,
/// `write_bbconfig`), so running BitBake through sous-chef does not rewrite `local.conf`.
/// It does not skip `repo_setup_loop`, which fetches and checks out the repositories that
/// `{repo, file}` includes name (kas 5.5 `libcmds.py`, `libkas.py`
/// `setup_parser_keep_config_unchanged_arg`), so kas mode is not used for a project with
/// such an include ([`BbEnv::probe`]).
pub const KAS_SHELL_ARGS: [&str; 2] = ["shell", "--keep-config-unchanged"];

/// How a kas-mode command is started: `<program> shell --keep-config-unchanged
/// <files joined by :> -c '<command>'` in `work_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KasRun {
    /// The `kas` executable.
    pub program: PathBuf,
    /// The kas files, absolute, in command-line order.
    pub files: Vec<PathBuf>,
    /// kas's work directory (the command's working directory).
    pub work_dir: PathBuf,
}

/// How long a terminated process group gets between SIGTERM and SIGKILL.
pub const KILL_GRACE: Duration = Duration::from_secs(2);

/// How long [`Supervised::terminate`] waits, after SIGKILL, for the group to be empty. A
/// killed process is gone once the kernel has scheduled its exit, and a member whose parent
/// was the leader is reaped by init; on a loaded machine that took over 20 ms in tests.
pub const GROUP_EXIT_WAIT: Duration = Duration::from_secs(5);

/// A usable BitBake environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BbEnv {
    pub build_dir: PathBuf,
    /// `None` when the process environment is already initialised.
    pub init_script: Option<PathBuf>,
    /// Set in kas mode: commands run through `kas shell`; `init_script` is `None` then.
    pub kas: Option<KasRun>,
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
    /// 4. A kas project with kas files (`ws.kas`), an executable `kas` on `PATH`, an
    ///    existing kas build directory and no `{repo, file}` include
    ///    (`KasProject::repo_includes`, see [`KAS_SHELL_ARGS`]): kas mode. With such an
    ///    include, the modes below are tried and the reason is kept for the error.
    /// 5. A build directory holding a `bitbake-setup` `init-build-env`: bitbake-setup mode.
    /// 6. A build directory and an `oe-init-build-env` in a layer root, a layer root's
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
                kas: None,
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
                    kas: None,
                });
            }
        }

        let mut kas_problem = None;
        if let Some(k) = ws.kas.as_ref().filter(|k| !k.files.is_empty()) {
            match find_on_path("kas", host.path.as_deref()) {
                Some(_) if !k.repo_includes.is_empty() => {
                    kas_problem = Some(format!(
                        "kas mode is not used: the kas files include files from repo {}, and kas shell --keep-config-unchanged still fetches and checks out such repositories",
                        k.repo_includes.join(", ")
                    ));
                }
                Some(program) if k.build_dir.is_dir() => {
                    return Ok(BbEnv {
                        build_dir: k.build_dir.clone(),
                        init_script: None,
                        kas: Some(KasRun {
                            program,
                            files: k.files.clone(),
                            work_dir: k.work_dir.clone(),
                        }),
                    });
                }
                Some(_) => {
                    kas_problem = Some(format!(
                        "the kas build directory {} does not exist yet; run kas checkout {}",
                        k.build_dir.display(),
                        k.files_arg(&k.work_dir)
                    ));
                }
                None => {
                    kas_problem = Some("this is a kas project but kas is not on PATH".to_string());
                }
            }
        }

        let Some(build_dir) = build_dir else {
            if let Some(p) = kas_problem {
                return Err(p);
            }
            return Err(
                "no build directory and no initialised environment; set build_dir or start sous-chef after sourcing oe-init-build-env"
                    .into(),
            );
        };
        let setup = build_dir.join(SETUP_SCRIPT_NAME);
        if setup.is_file() {
            return Ok(BbEnv {
                build_dir,
                init_script: Some(setup),
                kas: None,
            });
        }
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
                kas: None,
            }),
            None => Err(match kas_problem {
                Some(p) => format!(
                    "{p}, and no {INIT_SCRIPT_NAME} was found next to the layers; install kas or set init_script in sous-chef.toml"
                ),
                None => format!(
                    "no {INIT_SCRIPT_NAME} found next to the layers; set init_script in sous-chef.toml"
                ),
            }),
        }
    }

    /// `init-script`, `bitbake-setup`, `kas` or `initialised`.
    pub fn mode(&self) -> &'static str {
        if self.kas.is_some() {
            "kas"
        } else if self.is_setup_script() {
            "bitbake-setup"
        } else if self.init_script.is_some() {
            "init-script"
        } else {
            "initialised"
        }
    }

    /// Whether the init script is a `bitbake-setup` `init-build-env`.
    fn is_setup_script(&self) -> bool {
        self.init_script
            .as_deref()
            .and_then(Path::file_name)
            .is_some_and(|n| n == SETUP_SCRIPT_NAME)
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
        let mut dir = self.build_dir.clone();
        let mut cmd = match (&self.kas, &self.init_script) {
            (Some(k), _) => {
                let script = kas_command_string(program, args)?;
                let files = kas_files_arg(&k.files)?;
                let mut c = Command::new(&k.program);
                c.args(KAS_SHELL_ARGS).arg(files).arg("-c").arg(script);
                dir = k.work_dir.clone();
                c
            }
            (None, Some(script)) if self.is_setup_script() => {
                let mut c = Command::new("bash");
                c.arg("-c")
                    .arg(SETUP_WRAPPER)
                    .arg("--")
                    .arg(script)
                    .arg(program)
                    .args(args);
                c
            }
            (None, Some(script)) => {
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
            (None, None) => {
                let mut c = Command::new(program);
                c.args(args);
                c
            }
        };
        cmd.current_dir(&dir)
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

/// The command string kas mode hands to `kas shell -c`: the allowlisted `program` and each
/// argument, every one validated with [`validate_arg`] (so none holds a quote, `$`, a
/// backquote, a space or any other shell character) and wrapped in single quotes, joined
/// by spaces. Errors as [`BbEnv::command`] does; nothing is started.
pub fn kas_command_string(program: &str, args: &[&str]) -> Result<String> {
    if !ALLOWED_PROGRAMS.contains(&program) {
        return Err(Error::Invalid {
            msg: format!("program {program:?} is not allowed"),
        });
    }
    let mut words = vec![format!("'{program}'")];
    for a in args {
        validate_arg(a)?;
        // validate_arg admits no quote; checked again so the quoting can never be broken.
        if a.contains('\'') {
            return Err(Error::Invalid {
                msg: format!("invalid argument {a:?}"),
            });
        }
        words.push(format!("'{a}'"));
    }
    Ok(words.join(" "))
}

/// The kas files as one command-line argument, `a.yaml:b.yaml`. A path containing `:` (the
/// separator) cannot be passed and is an error.
pub fn kas_files_arg(files: &[PathBuf]) -> Result<OsString> {
    let mut out = OsString::new();
    for (i, f) in files.iter().enumerate() {
        if f.as_os_str().to_string_lossy().contains(':') {
            return Err(Error::Invalid {
                msg: format!(
                    "kas file {} contains `:`, which kas takes as a file separator",
                    f.display()
                ),
            });
        }
        if i > 0 {
            out.push(":");
        }
        out.push(f.as_os_str());
    }
    if out.is_empty() {
        return Err(Error::Invalid {
            msg: "kas mode has no kas files".into(),
        });
    }
    Ok(out)
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

    /// Whether any process (a zombie included) is still in the group: `kill(-pgid, 0)`
    /// succeeds, or fails with anything but `ESRCH` (`EPERM`: a member we may not signal).
    /// Without a group id (or off Unix), false.
    pub fn exists(&self) -> bool {
        self.pgid.is_some_and(sys::group_exists)
    }

    /// Polls until no process is left in the group, at most `timeout`. Returns whether the
    /// group was seen empty.
    pub async fn wait_gone(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if !self.exists() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// Whether a process with this pid exists (`kill(pid, 0)` succeeds or fails with `EPERM`).
/// True for an unusable pid (0, 1, out of range) and off Unix, so a caller that removes
/// what belongs to a dead process never removes anything it cannot check.
pub fn process_alive(pid: u32) -> bool {
    sys::process_alive(pid)
}

/// A spawned command that owns its process group. Waiting with a timeout or a cancel
/// future terminates the group ([`Supervised::terminate`]). Dropping it before the process
/// was reaped sends SIGKILL to the group and waits for nothing (this is what happens to a
/// running job when the server exits and the runtime drops its task).
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
    /// timeout or cancellation the group is terminated ([`Supervised::terminate`]) before
    /// this returns: the leader has been reaped and the group was seen empty, or
    /// [`GROUP_EXIT_WAIT`] passed after SIGKILL. When the process exits on its own, the rest
    /// of its group is left alone.
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
                // A member that outlives the wait (one in uninterruptible sleep, say) is not
                // reported; SIGKILL has been sent to it.
                let _gone = self.terminate().await?;
                Ok(finish)
            }
        }
    }

    /// SIGTERM to the group, up to [`KILL_GRACE`] for the leader to exit, then SIGKILL to
    /// the group (for anything that ignored SIGTERM), reap the leader, and wait up to
    /// [`GROUP_EXIT_WAIT`] until no process is left in the group. Signals are delivered
    /// asynchronously and only the leader is our child, so the other members may still be
    /// exiting when SIGKILL has been sent; returns whether the group was seen empty.
    pub async fn terminate(&mut self) -> Result<bool> {
        self.group.terminate();
        let _ = tokio::time::timeout(KILL_GRACE, self.child.wait()).await;
        self.group.kill();
        self.child.wait().await?;
        self.reaped = true;
        Ok(self.group.wait_gone(GROUP_EXIT_WAIT).await)
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
    /// `ESRCH`, "no such process": 3 on Linux and the BSDs (`errno.h`).
    const ESRCH: i32 = 3;

    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    /// `kill(target, 0)`: true unless it fails with `ESRCH`.
    fn exists(target: i32) -> bool {
        // SAFETY: as in `signal_group`; signal 0 sends nothing, it only checks.
        let r = unsafe { kill(target, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() != Some(ESRCH)
    }

    /// Whether a process is in group `pgid` (module docs of `ProcessGroup::exists`).
    pub fn group_exists(pgid: u32) -> bool {
        match i32::try_from(pgid) {
            Ok(p) if p > 1 => exists(-p),
            _ => false,
        }
    }

    /// Whether process `pid` exists; true for a pid that cannot be checked.
    pub fn process_alive(pid: u32) -> bool {
        match i32::try_from(pid) {
            Ok(p) if p > 1 => exists(p),
            _ => true,
        }
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
    pub fn group_exists(_pgid: u32) -> bool {
        false
    }
    pub fn process_alive(_pid: u32) -> bool {
        true
    }
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

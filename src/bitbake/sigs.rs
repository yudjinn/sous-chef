//! "Why did this task re-run?": a digest of `bitbake-diffsigs` output. See the `sigs` tool
//! in docs/mcp-tools.md and "Task signatures" in docs/bitbake-semantics.md.
//!
//! BitBake writes a task's signature data (`<stamp>.<task>.sigdata.<hash>` in the stamps
//! directory, `*.siginfo` in the sstate cache) as **zstd-compressed JSON** on Kirkstone
//! (BitBake 2.0), Scarthgap (2.8) and Wrynose (2.18) alike (`lib/bb/siggen.py`
//! `dump_sigtask`, `compare_sigfiles`). Reading it needs a zstd decoder, which is not a
//! dependency of sous-chef, so this module does not read signature files: it runs
//! `bitbake-diffsigs` (through a [`DiffSigs`] runner) and digests what it prints, the way
//! `logs` digests a task log. The comparison is BitBake's own.
//!
//! [`parse`] turns the output into [`Item`]s with their recursion depth (`bitbake-diffsigs`
//! indents a dependency's comparison by four spaces under `Hash for task dependency X
//! changed from A to B`, and drops what it had printed before, so the deepest items are
//! the root cause).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use crate::bitbake::{BbEnv, Finish};
use crate::error::Result;

/// The program the digest is made from.
pub const PROGRAM: &str = "bitbake-diffsigs";

/// How long `bitbake-diffsigs` may run (it starts a config-only tinfoil and may follow
/// several dependencies).
pub const TIMEOUT: Duration = Duration::from_secs(180);

/// What a runner returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    /// The exit status; `None` when it was killed (timeout).
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Runs `bitbake-diffsigs` with the given arguments. The tool uses [`EnvDiffSigs`]; tests
/// use a fake script.
pub trait DiffSigs: Sync {
    fn diffsigs<'a>(
        &'a self,
        args: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<RunOutput>> + Send + 'a>>;
}

/// [`DiffSigs`] through the build environment's runner ([`BbEnv::output`]), which runs only
/// the programs of [`crate::bitbake::env::ALLOWED_PROGRAMS`].
pub struct EnvDiffSigs {
    pub env: BbEnv,
}

impl EnvDiffSigs {
    /// Whether the environment runner allows [`PROGRAM`].
    pub fn allowed() -> bool {
        crate::bitbake::env::ALLOWED_PROGRAMS.contains(&PROGRAM)
    }
}

impl DiffSigs for EnvDiffSigs {
    fn diffsigs<'a>(
        &'a self,
        args: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<RunOutput>> + Send + 'a>> {
        Box::pin(async move {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = self.env.output(PROGRAM, &refs, Some(TIMEOUT)).await?;
            let (status, timed_out) = match out.finish {
                Finish::Exited(s) => (s.code(), false),
                Finish::TimedOut => (None, true),
                Finish::Cancelled => (None, false),
            };
            Ok(RunOutput {
                status,
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                timed_out,
            })
        })
    }
}

/// One change `compare_sigfiles` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// `Variable X value changed ...`: the old and new value when printed on one line, else
    /// the word diff (`"a [-b-] {+c+}"`) or the unified diff lines that follow.
    VarValue {
        var: String,
        old: Option<String>,
        new: Option<String>,
        diff: Vec<String>,
    },
    /// `Dependency on variable X was added` / `... Variable X was removed`.
    VarDep { var: String, added: bool },
    /// `List of dependencies for variable X changed from '...' to '...'`.
    VarDeps { var: String, items: Option<String> },
    /// `basehash changed from A to B`.
    BaseHash { old: String, new: String },
    /// `Task dependencies changed from:` (the variables the task depends on), with the two
    /// lists that follow.
    TaskDeps { lines: Vec<String> },
    /// `Checksum for file F changed from A to B`.
    FileChecksum { file: String },
    /// `Dependency on checksum of file F was added` / `removed`.
    FileDep { file: String, added: bool },
    /// `Dependency on task T was added` / `removed with hash H`.
    TaskDep { task: String, added: bool },
    /// `Hash for task dependency T changed from A to B`: what follows, indented, is T's own
    /// comparison.
    DepHash {
        task: String,
        old: String,
        new: String,
    },
    /// `runtaskdeps changed:` / `Number of task dependencies changed`.
    RunTaskDeps { lines: Vec<String> },
    /// `Taint (by forced/invalidated task) changed from A to B`.
    Taint { old: String, new: String },
    /// `basehash_ignore_vars` / `taskhash_ignore_tasks changed from ...`.
    IgnoreList { which: String },
    /// `Unable to find matching sigdata for K with hash(es) ...`: recursion stopped.
    Unmatched { text: String },
}

/// A change and where it is in the recursion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// 0 for the task compared, 1 for a dependency's comparison, ...
    pub depth: usize,
    pub change: Change,
    /// The `DepHash` tasks above it, outermost first.
    pub chain: Vec<String>,
}

/// Why there is nothing to compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoDiff {
    /// `Only one matching sigdata file found for the specified task (pn task)`.
    OnlyOne,
    /// `No sigdata files found matching pn task`.
    NoneFound,
}

/// Parsed `bitbake-diffsigs` output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub items: Vec<Item>,
    pub no_diff: Option<NoDiff>,
    /// `ERROR:` lines that are not one of the above.
    pub errors: Vec<String>,
    /// Lines no pattern recognised (kept out of the digest, counted).
    pub unknown: usize,
}

struct Patterns {
    dep_hash: Regex,
    basehash: Regex,
    var_inline: Regex,
    var_block: Regex,
    var_deps: Regex,
    var_dep: Regex,
    task_deps: Regex,
    file_sum: Regex,
    file_dep: Regex,
    task_dep: Regex,
    runtaskdeps: Regex,
    taint: Regex,
    ignore: Regex,
    unmatched: Regex,
    only_one: Regex,
    none_found: Regex,
    ansi: Regex,
}

static PATTERNS: LazyLock<Option<Patterns>> = LazyLock::new(|| {
    Some(Patterns {
        dep_hash: Regex::new(r"^Hash for task dependency (\S+) changed from (\S+) to (\S+)$")
            .ok()?,
        basehash: Regex::new(r"^basehash changed from (\S+) to (\S+)$").ok()?,
        var_inline: Regex::new(r"^Variable (\S+) value changed from '(.*)' to '(.*)'$").ok()?,
        var_block: Regex::new(r"^Variable (\S+) value changed:$").ok()?,
        var_deps: Regex::new(r"^List of dependencies for variable (\S+) changed from").ok()?,
        var_dep: Regex::new(r"^Dependency on [vV]ariable (\S+) was (added|removed)$").ok()?,
        task_deps: Regex::new(r"^Task dependencies changed from:$").ok()?,
        file_sum: Regex::new(r"^Checksum for file (.+) changed from (\S+) to (\S+)$").ok()?,
        file_dep: Regex::new(r"^Dependency on checksum of file (.+) was (added|removed)$").ok()?,
        task_dep: Regex::new(r"^Dependency on task (\S+) was (added|removed) with hash (\S+)$")
            .ok()?,
        runtaskdeps: Regex::new(r"^(runtaskdeps changed:|Number of task dependencies changed)")
            .ok()?,
        taint: Regex::new(r"^Taint \(by forced/invalidated task\) changed from (.*) to (.*)$")
            .ok()?,
        ignore: Regex::new(r"^(basehash_ignore_vars|taskhash_ignore_tasks) changed from").ok()?,
        unmatched: Regex::new(r"^Unable to find matching sigdata for ").ok()?,
        only_one: Regex::new(r"Only one matching sigdata file found for the specified task")
            .ok()?,
        none_found: Regex::new(r"No sigdata files found matching ").ok()?,
        ansi: Regex::new(r"\x1b\[[0-9;]*m").ok()?,
    })
});

/// Parses `bitbake-diffsigs` stdout and stderr.
pub fn parse(stdout: &str, stderr: &str) -> Diff {
    let mut d = Diff::default();
    let Some(p) = PATTERNS.as_ref() else {
        return d;
    };
    for line in stderr.lines().chain(stdout.lines()) {
        let line = p.ansi.replace_all(line, "");
        if p.only_one.is_match(&line) {
            d.no_diff = Some(NoDiff::OnlyOne);
        } else if p.none_found.is_match(&line) {
            d.no_diff = Some(NoDiff::NoneFound);
        } else if let Some(rest) = line.trim_start().strip_prefix("ERROR: ") {
            d.errors.push(rest.to_string());
        }
    }
    // The DepHash chain open at each depth.
    let mut chain: Vec<(usize, String)> = Vec::new();
    for raw in stdout.lines() {
        let line = p.ansi.replace_all(raw, "");
        if line.trim().is_empty() {
            continue;
        }
        let spaces = line.len() - line.trim_start_matches(' ').len();
        let depth = spaces / 4;
        let text = line.get(depth * 4..).unwrap_or("").trim_end();
        chain.retain(|(dd, _)| *dd < depth);
        let current: Vec<String> = chain.iter().map(|(_, t)| t.clone()).collect();
        let change = if let Some(c) = p.dep_hash.captures(text) {
            let task = c[1].to_string();
            chain.push((depth, task.clone()));
            Some(Change::DepHash {
                task,
                old: c[2].to_string(),
                new: c[3].to_string(),
            })
        } else if let Some(c) = p.basehash.captures(text) {
            Some(Change::BaseHash {
                old: c[1].to_string(),
                new: c[2].to_string(),
            })
        } else if let Some(c) = p.var_inline.captures(text) {
            Some(Change::VarValue {
                var: c[1].to_string(),
                old: Some(c[2].to_string()),
                new: Some(c[3].to_string()),
                diff: Vec::new(),
            })
        } else if let Some(c) = p.var_block.captures(text) {
            Some(Change::VarValue {
                var: c[1].to_string(),
                old: None,
                new: None,
                diff: Vec::new(),
            })
        } else if let Some(c) = p.var_deps.captures(text) {
            Some(Change::VarDeps {
                var: c[1].to_string(),
                items: None,
            })
        } else if let Some(c) = p.var_dep.captures(text) {
            Some(Change::VarDep {
                var: c[1].to_string(),
                added: &c[2] == "added",
            })
        } else if p.task_deps.is_match(text) {
            Some(Change::TaskDeps { lines: Vec::new() })
        } else if let Some(c) = p.file_sum.captures(text) {
            Some(Change::FileChecksum {
                file: c[1].to_string(),
            })
        } else if let Some(c) = p.file_dep.captures(text) {
            Some(Change::FileDep {
                file: c[1].to_string(),
                added: &c[2] == "added",
            })
        } else if let Some(c) = p.task_dep.captures(text) {
            Some(Change::TaskDep {
                task: c[1].to_string(),
                added: &c[2] == "added",
            })
        } else if p.runtaskdeps.is_match(text) {
            Some(Change::RunTaskDeps { lines: Vec::new() })
        } else if let Some(c) = p.taint.captures(text) {
            Some(Change::Taint {
                old: c[1].to_string(),
                new: c[2].to_string(),
            })
        } else if let Some(c) = p.ignore.captures(text) {
            Some(Change::IgnoreList {
                which: c[1].to_string(),
            })
        } else if p.unmatched.is_match(text) {
            Some(Change::Unmatched {
                text: text.to_string(),
            })
        } else {
            None
        };
        match change {
            Some(change) => d.items.push(Item {
                depth,
                change,
                chain: current,
            }),
            None => {
                // A continuation of the previous change at this depth (a word diff, the
                // lines of a unified diff, the lists of a dependency change), else unknown.
                match d.items.last_mut() {
                    Some(prev) if prev.depth == depth => match &mut prev.change {
                        Change::VarValue { diff, .. } => diff.push(text.to_string()),
                        Change::TaskDeps { lines } | Change::RunTaskDeps { lines } => {
                            lines.push(text.to_string())
                        }
                        Change::VarDeps { items, .. } => {
                            if let Some(rest) = text.strip_prefix("changed items: ") {
                                *items = Some(rest.to_string());
                            }
                        }
                        _ => d.unknown += 1,
                    },
                    _ => d.unknown += 1,
                }
            }
        }
    }
    d
}

impl Diff {
    /// The deepest recursion level reached.
    pub fn max_depth(&self) -> usize {
        self.items.iter().map(|i| i.depth).max().unwrap_or(0)
    }

    /// The changes that are causes, not "a dependency's hash changed" (which only leads to
    /// one): deepest first, in output order within a depth.
    pub fn causes(&self) -> Vec<&Item> {
        let mut out: Vec<&Item> = self
            .items
            .iter()
            .filter(|i| !matches!(i.change, Change::DepHash { .. }))
            .collect();
        out.sort_by_key(|i| std::cmp::Reverse(i.depth));
        out
    }

    /// The `DepHash` steps leading to the deepest cause, outermost first, with their hashes.
    pub fn path(&self) -> Vec<(&str, &str, &str)> {
        let Some(deepest) = self.causes().first().map(|i| i.chain.clone()) else {
            // No cause below: the chain of DepHash items as printed.
            return self
                .items
                .iter()
                .filter_map(|i| match &i.change {
                    Change::DepHash { task, old, new } => {
                        Some((task.as_str(), old.as_str(), new.as_str()))
                    }
                    _ => None,
                })
                .collect();
        };
        let mut out = Vec::new();
        for t in &deepest {
            if let Some((task, old, new)) = self.items.iter().find_map(|i| match &i.change {
                Change::DepHash { task, old, new } if task == t => {
                    Some((task.as_str(), old.as_str(), new.as_str()))
                }
                _ => None,
            }) {
                out.push((task, old, new));
            }
        }
        out
    }

    /// Whether the compared task's own basehash changed (a `basehash changed` at depth 0).
    pub fn own_basehash_changed(&self) -> bool {
        self.items
            .iter()
            .any(|i| i.depth == 0 && matches!(i.change, Change::BaseHash { .. }))
    }
}

/// The recipe name and task of a dependency as `bitbake-diffsigs` names it: `pn:do_task`
/// or `mc:<mc>:pn:do_task` (BitBake 2.8 and later, `build_pnid`), or on BitBake 2.0
/// (Kirkstone) the cleaned recipe path `dir/pn_pv.bb:do_task` with a
/// `:virtual:<class>[:<variant>]` suffix for a class-extended variant (`clean_basepath`).
/// The variant is named as its recipe is (`pn-native`, `nativesdk-pn`, `lib32-pn`).
pub fn split_task(dep: &str) -> (Option<String>, String) {
    let mut s = dep;
    if let Some(rest) = s.strip_prefix("mc:") {
        s = rest.split_once(':').map_or(rest, |(_, r)| r);
    }
    if let Some((path, rest)) = s.split_once(".bb:") {
        let file = path.rsplit('/').next().unwrap_or(path);
        let pn = file.split('_').next().unwrap_or(file).to_string();
        let mut parts = rest.split(':');
        let task = parts.next().unwrap_or("").to_string();
        let tail: Vec<&str> = parts.collect();
        let pn = match tail.as_slice() {
            ["virtual", "native", ..] => format!("{pn}-native"),
            ["virtual", "nativesdk", ..] => format!("nativesdk-{pn}"),
            ["virtual", "multilib", v, ..] => format!("{v}-{pn}"),
            _ => pn,
        };
        return (Some(pn), task);
    }
    match s.rsplit_once(':') {
        Some((pn, task)) if task.starts_with("do_") => (Some(pn.to_string()), task.to_string()),
        _ => (None, s.to_string()),
    }
}

/// The words a word diff (`"a [-b-] {+c+}"`) adds.
pub fn added_words(diff: &[String]) -> Vec<String> {
    let Ok(re) = Regex::new(r"\{\+(.*?)\+\}") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for l in diff {
        for c in re.captures_iter(l) {
            out.extend(c[1].split_whitespace().map(str::to_string));
        }
    }
    // Unified diff lines.
    for l in diff {
        if let Some(rest) = l.strip_prefix('+')
            && !rest.starts_with("++")
        {
            out.extend(rest.split_whitespace().map(str::to_string));
        }
    }
    out
}

/// The signature files of `pn`'s `task` in the stamps directories of the build directory:
/// `<build>/tmp*/stamps/*/<pn>/*.<task>.sigdata.*` (`STAMP =
/// "${STAMPS_DIR}/${MULTIMACH_TARGET_SYS}/${PN}/${PV}"`, `STAMPS_DIR ?= "${TMPDIR}/stamps"`,
/// `dump_sigtask` naming). `confine` decides which directories may be listed.
pub fn stamp_sigfiles(
    build_dir: &Path,
    pn: &str,
    task: &str,
    confine: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let needle = format!(".{task}.sigdata.");
    let Ok(rd) = std::fs::read_dir(build_dir) else {
        return out;
    };
    for tmp in rd.flatten().map(|e| e.path()) {
        let is_tmp = tmp
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("tmp"));
        let stamps = tmp.join("stamps");
        if !is_tmp || !confine(&stamps) {
            continue;
        }
        let Ok(archs) = std::fs::read_dir(&stamps) else {
            continue;
        };
        for arch in archs.flatten() {
            let dir = arch.path().join(pn);
            if !confine(&dir) {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for f in files.flatten() {
                if f.file_name().to_string_lossy().contains(&needle) {
                    out.push(f.path());
                }
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_names() {
        assert_eq!(
            split_task("curl:do_configure"),
            (Some("curl".into()), "do_configure".into())
        );
        assert_eq!(
            split_task("mc:a:zlib-native:do_install"),
            (Some("zlib-native".into()), "do_install".into())
        );
        assert_eq!(
            split_task("zlib/zlib_1.2.11.bb:do_populate_sysroot:virtual:native"),
            (Some("zlib-native".into()), "do_populate_sysroot".into())
        );
        assert_eq!(
            split_task("curl/curl_7.82.0.bb:do_compile"),
            (Some("curl".into()), "do_compile".into())
        );
    }

    #[test]
    fn nested() {
        let out = "\
Hash for task dependency hello:do_configure changed from aa to bb
    basehash changed from 11 to 22
    Variable EXTRA_OECONF value changed:
    \"--disable-nls [---with-old-] {+--with-mine+}\"
";
        let d = parse(out, "");
        assert_eq!(d.max_depth(), 1);
        let causes = d.causes();
        assert_eq!(causes.len(), 2);
        assert_eq!(causes[0].chain, vec!["hello:do_configure"]);
        let Change::VarValue { var, diff, .. } = &causes[1].change else {
            panic!("{causes:?}")
        };
        assert_eq!(var, "EXTRA_OECONF");
        assert_eq!(added_words(diff), vec!["--with-mine"]);
        assert_eq!(d.path(), vec![("hello:do_configure", "aa", "bb")]);
        assert_eq!(d.unknown, 0);
    }
}

//! Checks against the output of the first real build (y1-build, 2026-11-18: `bitbake curl`
//! on the kas build of yocto-kiss; docs/live-verification.md, "The first real build").
//! Every input is a recorded, path-scrubbed copy under `tests/fixtures/outputs/live/y1-build/`
//! (see its README entry); nothing here needs BitBake or the live directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use sous_chef::bitbake::pkgdata::{self, Found, PkgData};
use sous_chef::error::Result;

fn sample_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/outputs/live/y1-build")
}

fn sample(name: &str) -> String {
    fs::read_to_string(sample_dir().join(name)).unwrap()
}

/// What `Workspace::confine` does for a path inside the workspace: the canonical path
/// (symlinks resolved, which the `runtime-reverse/` lookup relies on).
fn open_dir(p: &Path) -> Result<PathBuf> {
    Ok(fs::canonicalize(p)?)
}

/// A path as `oe-pkgdata-util find-path` prints it: it `json.loads` the raw `FILES_INFO`
/// value, so the `\uXXXX` escapes `json.dumps` wrote stay escaped (one backslash).
fn as_oe_prints(path: &str) -> String {
    let mut out = String::new();
    for c in path.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}

/// `find_path` over the real package data of curl and ca-certificates gives the same
/// `(package, path)` pairs as `oe-pkgdata-util find-path '*'` (1062 lines), once the one
/// escaped name is compared as oe-pkgdata-util prints it.
#[test]
fn real_pkgdata_find_path_equals_oe_pkgdata_util() {
    let dir = sample_dir().join("pkgdata/dogbonedark");
    let pd = PkgData::new(dir, &open_dir);
    assert!(pd.present());
    let re = pkgdata::glob_regex("*").unwrap();
    let ours: BTreeSet<String> = pd
        .find_path(&re)
        .into_iter()
        .map(|(p, path)| format!("{}: {}", p.name, as_oe_prints(&path)))
        .collect();
    let theirs: BTreeSet<String> = sample("oe-pkgdata-util-find-path.out")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(theirs.len(), 1062);
    assert_eq!(ours, theirs);
    // The decoded name is the file's real name on the target.
    let hit = pd
        .find_path(&pkgdata::glob_regex("*NetLock*").unwrap())
        .into_iter()
        .map(|(_, p)| p)
        .collect::<Vec<_>>();
    assert_eq!(
        hit,
        vec!["/usr/share/ca-certificates/mozilla/NetLock_Arany_=Class_Gold=_Főtanúsítvány.crt"]
    );
}

/// Sizes: `FILES_INFO` sizes equal `oe-pkgdata-util list-pkg-files -l -p curl`, and
/// `PKGSIZE` is their sum (no hard links here).
#[test]
fn real_pkgdata_sizes_equal_list_pkg_files_long() {
    let dir = sample_dir().join("pkgdata/dogbonedark");
    let pd = PkgData::new(dir, &open_dir);
    let mut theirs: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut current = String::new();
    for line in sample("oe-pkgdata-util-list-pkg-files-l-curl.out").lines() {
        if let Some(pkg) = line.strip_suffix(':')
            && !line.starts_with('\t')
        {
            current = pkg.to_string();
            theirs.entry(current.clone()).or_default();
        } else if let Some(rest) = line.strip_prefix('\t') {
            let (path, size) = rest.rsplit_once('\t').unwrap();
            theirs
                .get_mut(&current)
                .unwrap()
                .insert(path.trim_end().to_string(), size.trim().parse().unwrap());
        }
    }
    // list-pkg-files -p lists the packaged packages only.
    assert_eq!(
        theirs.keys().cloned().collect::<Vec<_>>(),
        [
            "curl", "curl-dbg", "curl-dev", "curl-doc", "curl-src", "libcurl"
        ]
    );
    for (pkg, files) in &theirs {
        let p = pd.package(pkg).unwrap();
        assert!(p.packaged, "{pkg}");
        let ours = p.files().unwrap();
        assert_eq!(&ours, files, "{pkg}");
        let sum: u64 = ours.values().sum();
        assert_eq!(p.get("PKGSIZE"), Some(sum.to_string().as_str()), "{pkg}");
    }
}

/// Renamed package and lookups, as `oe-pkgdata-util lookup-recipe`, `package-info` and
/// `list-pkgs -p curl [-r]` answered on the real build.
#[test]
fn real_pkgdata_renamed_libcurl_and_lookups() {
    let dir = sample_dir().join("pkgdata/dogbonedark");
    let pd = PkgData::new(dir, &open_dir);
    // libcurl is renamed by debian.bbclass: PKG:libcurl = libcurl4.
    let found = pd.lookup("libcurl4");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0.name, "libcurl");
    assert_eq!(found[0].1, Found::Reverse);
    assert_eq!(found[0].0.runtime_name(), "libcurl4");
    // `libcurl` is in its own RPROVIDES (`RPROVIDES:libcurl:  libcurl (=8.19.0)`): the
    // runtime-rprovides directory is named without the version constraint.
    let found = pd.lookup("libcurl");
    assert_eq!(found[0].1, Found::RProvides("libcurl".into()));
    assert_eq!(found[0].0.get("RPROVIDES"), Some("libcurl (=8.19.0)"));
    for name in ["curl", "libcurl", "libcurl4", "curl-dev"] {
        let p = &pd.lookup(name)[0].0;
        assert_eq!(p.get("PN"), Some("curl"), "{name}");
    }
    // package-info: `<pkg> 8.19.0-r0 curl 8.19.0-r0 <PKGSIZE>`.
    let info = sample("oe-pkgdata-util.out");
    for (name, size) in [
        ("curl", "156605"),
        ("libcurl", "432452"),
        ("libcurl4", "432452"),
    ] {
        assert!(info.contains(&format!("{name} 8.19.0-r0 curl 8.19.0-r0 {size}\n")));
        assert_eq!(pd.lookup(name)[0].0.get("PKGSIZE"), Some(size));
    }
    // list-pkgs -p curl: the packaged ones of PACKAGES; -r gives the runtime names.
    let packages = pd.recipe_packages("curl").unwrap();
    assert_eq!(
        packages,
        [
            "curl-src",
            "curl-dbg",
            "libcurl",
            "curl-staticdev",
            "curl-dev",
            "curl-doc",
            "curl-locale",
            "curl"
        ]
    );
    let mut packaged: Vec<String> = Vec::new();
    let mut runtime: Vec<String> = Vec::new();
    for n in &packages {
        let p = pd.package(n).unwrap();
        if p.packaged {
            packaged.push(n.clone());
            runtime.push(p.runtime_name().to_string());
        }
    }
    packaged.sort();
    runtime.sort();
    assert_eq!(
        packaged,
        [
            "curl", "curl-dbg", "curl-dev", "curl-doc", "curl-src", "libcurl"
        ]
    );
    assert_eq!(
        runtime,
        [
            "curl", "curl-dbg", "curl-dev", "curl-doc", "curl-src", "libcurl4"
        ]
    );
}

// ---------------------------------------------------------------------------------------
// sigs: `EXTRA_OECONF:append = " --disable-dict"` in a scratch bbappend, curl rebuilt.

use sous_chef::bitbake::sigs::{self, Change};

/// The real `bitbake-diffsigs -t curl do_configure` after the change: the task's own
/// basehash and `EXTRA_OECONF`, printed as a unified diff (the value starts with a newline)
/// whose `+` line is the whole value; the word added is `--disable-dict` alone.
#[test]
fn real_diffsigs_configure_unified_diff_word_added() {
    let d = sigs::parse(&sample("sigs/diffsigs-curl-do_configure.out"), "");
    assert_eq!(d.unknown, 0, "{d:?}");
    assert!(d.own_basehash_changed());
    assert_eq!(d.max_depth(), 0);
    let causes = d.causes();
    let Change::VarValue { var, diff, old, .. } = &causes
        .iter()
        .find(|i| matches!(i.change, Change::VarValue { .. }))
        .unwrap()
        .change
    else {
        unreachable!()
    };
    assert_eq!(var, "EXTRA_OECONF");
    assert!(old.is_none());
    assert_eq!(diff[0], "@@ -1,4 +1,4 @@");
    // The context lines are the PACKAGECONFIG words the value's python tests.
    assert_eq!(diff.last().unwrap(), " PACKAGECONFIG{openssl} = Set");
    assert_eq!(
        sigs::word_changes(diff),
        (vec!["--disable-dict".to_string()], Vec::<String>::new())
    );
}

/// `do_compile` and `do_install` re-ran because of `do_configure`: the recursion is
/// indented four spaces per level, the root cause is the same `EXTRA_OECONF` change.
#[test]
fn real_diffsigs_compile_and_install_chain() {
    let d = sigs::parse(&sample("sigs/diffsigs-curl-do_compile.out"), "");
    assert_eq!(d.unknown, 0, "{d:?}");
    assert!(!d.own_basehash_changed());
    assert_eq!(
        d.path(),
        vec![(
            "curl:do_configure",
            "7ee05dbc4c2109710f0305e9cd4e5d8039f415e527fd25750d9eb14c37d610b6",
            "8a1a4041e93f80660ce296a8dc8805c8827549a7be2be702db5fb4c6cc39e6cd"
        )]
    );
    let d = sigs::parse(&sample("sigs/diffsigs-curl-do_install.out"), "");
    assert_eq!(d.unknown, 0, "{d:?}");
    assert_eq!(d.max_depth(), 2);
    let path: Vec<&str> = d.path().iter().map(|p| p.0).collect();
    assert_eq!(path, ["curl:do_compile", "curl:do_configure"]);
    assert_eq!(d.causes()[0].depth, 2);
    let var = d
        .causes()
        .into_iter()
        .find_map(|i| match &i.change {
            Change::VarValue { diff, .. } => Some(sigs::added_words(diff)),
            _ => None,
        })
        .unwrap();
    assert_eq!(var, ["--disable-dict"]);
}

/// When the previous client's server has not exited yet, `bitbake-diffsigs` prints
/// `NOTE: Reconnecting to bitbake server...` and `NOTE: Retrying server connection (#1)...`
/// lines first (seen live: 7 of them); they are not comparison output.
#[test]
fn real_diffsigs_client_notes_are_not_unknown_lines() {
    let text = sample("sigs/diffsigs-curl-do_compile-reconnect.out");
    assert!(text.starts_with("NOTE: Reconnecting to bitbake server..."));
    let d = sigs::parse(&text, "");
    assert_eq!(d.unknown, 0);
    assert_eq!(d.path().len(), 1);
}

// ---------------------------------------------------------------------------------------
// log: two real failures of curl, made on purpose from the scratch bbappend:
// `EXTRA_OECONF:append = " --with-libpsl"` (configure) and a `do_compile:prepend` that
// prints a compiler-style line and runs `false` (compile).

use sous_chef::bitbake::logs::{self, DigestInput, FailureKind};

/// The real `configure: error`: found first; the exit trap names `bbfatal_log` (innermost)
/// in `oe_runconf`, and the run file's `# line:` comment maps run-file line 255 to
/// `autotools.bbclass:96`, the `bbfatal_log "configure failed"` line.
#[test]
fn real_configure_failure_log_and_line_mapping() {
    let log = sample("log/log.do_configure.1749535");
    let run = sample("log/run.do_configure.1749535");
    let f = logs::detect(&log);
    let first = logs::first_error(&f).unwrap();
    assert_eq!(first.kind, FailureKind::Configure);
    assert_eq!(
        first.message,
        "configure: error: libpsl libs and/or directories were not found where specified!"
    );
    let trace = logs::shell_trace(&log).unwrap();
    assert_eq!(trace.line, 279);
    assert_eq!(trace.command, "exit 1");
    let frames: Vec<&str> = trace.frames.iter().map(|f| f.function.as_str()).collect();
    assert_eq!(
        frames,
        [
            "bbfatal_log",
            "oe_runconf",
            "autotools_do_configure",
            "do_configure",
            "main"
        ]
    );
    let func = logs::failing_function(&trace, Some(&run)).unwrap();
    assert_eq!(func.function, "oe_runconf");
    assert_eq!(func.run_line, 255);
    let (file, line) = func.source.unwrap();
    assert!(
        file.ends_with("openembedded-core/meta/classes-recipe/autotools.bbclass"),
        "{file:?}"
    );
    assert_eq!(line, 96);
    // The `do_configure` frame (run line 149) is in the EXPORT_FUNCTIONS stub.
    let (file, line) = logs::metadata_line(&run, 149).unwrap();
    assert_eq!((file.to_str().unwrap(), line), ("autogenerated", 3));
    let d = logs::digest(&DigestInput {
        text: &log,
        ..DigestInput::default()
    });
    let rendered = d.render(sous_chef::render::Format::Text, 400);
    assert!(rendered.len() < 1024, "{rendered}");
}

/// The compile failure: the compiler-style line is the first error, the shell trap is not
/// repeated as context, and the failing frame maps to `autogenerated`: BitBake writes the
/// `:prepend` inside the `do_compile` stub under its `# line: 1, file: autogenerated`
/// comment.
#[test]
fn real_compile_failure_in_prepend_maps_to_autogenerated() {
    let log = sample("log/log.do_compile.1757933");
    let run = sample("log/run.do_compile.1757933");
    let first = logs::first_error(&logs::detect(&log)).cloned().unwrap();
    assert_eq!(first.kind, FailureKind::Compile);
    assert_eq!(first.line, 2);
    let trace = logs::shell_trace(&log).unwrap();
    assert_eq!((trace.line, trace.command.as_str()), (148, "false"));
    let func = logs::failing_function(&trace, Some(&run)).unwrap();
    assert_eq!(func.function, "do_compile");
    let (file, line) = func.source.unwrap();
    assert_eq!((file.to_str().unwrap(), line), ("autogenerated", 3));
    assert!(run.contains(
        "# line: 1, file: autogenerated\ndo_compile() {\n\techo \"lib/sous_chef_probe.c"
    ));
    let d = logs::digest(&DigestInput {
        text: &log,
        ..DigestInput::default()
    });
    assert!(d.first.unwrap().context.is_empty());
}

/// The consoles: `Logfile of failure stored in:`, the runqueue's exit code, and the
/// `Log data follows:` block even though the task logged `ERROR: configure failed`
/// (`bbfatal_log` logs with `forcelog`, which does not suppress the log).
#[test]
fn real_failure_consoles() {
    for (name, task, log_name) in [
        (
            "log/console-configure-fail.out",
            "do_configure",
            "log.do_configure.1749535",
        ),
        (
            "log/console-compile-fail.out",
            "do_compile",
            "log.do_compile.1757933",
        ),
    ] {
        let console = sample(name);
        assert!(console.contains("\nLog data follows:\n"), "{name}");
        let tasks = logs::failed_tasks(&console);
        assert_eq!(tasks.len(), 1, "{name}");
        assert_eq!(tasks[0].task, task);
        assert_eq!(tasks[0].exit_code, Some(1));
        assert!(
            tasks[0]
                .recipe_file
                .ends_with("recipes-support/curl/curl_8.19.0.bb")
        );
        let log = tasks[0].log.clone().unwrap();
        assert!(log.ends_with(format!("curl/8.19.0/temp/{log_name}")));
        assert_eq!(logs::exit_from_console(&console, &log), Some(1));
        assert_eq!(logs::failure_logfiles(&console), vec![log]);
        let first = logs::first_error(&logs::detect(&console)).cloned().unwrap();
        assert!(matches!(
            first.kind,
            FailureKind::Configure | FailureKind::Compile
        ));
    }
    let console = sample("log/console-configure-fail.out");
    assert!(console.contains("ERROR: curl-8.19.0-r0 do_configure: configure failed\n"));
    assert!(console.contains("| ERROR: configure failed\n"));
}

/// The Bash hook recognises the command as the live environment runs it (`env -u
/// NETRC_FILE ../venv/bin/kas shell ... -c 'bitbake curl'`); it passed it through before.
#[test]
fn real_kas_command_with_env_unset_runs_bitbake() {
    assert!(sous_chef::hook::bash::runs_bitbake(
        "env -u NETRC_FILE ../venv/bin/kas shell ../scratch/kas-scratch.yaml -c 'bitbake curl'"
    ));
}

/// `log.task_order` after the first build, the two experiments and the failed runs:
/// `<YYYYMMDD-HHMMSS.ffffff> do_<task> (<pid>): log.do_<task>.<pid>`, one line per run,
/// appended (the fetch of 2026-11-17 is still first).
#[test]
fn real_log_task_order_format() {
    let text = sample("log/log.task_order");
    let re =
        regex::Regex::new(r"^\d{8}-\d{6}\.\d{6} (do_[a-z_]+) \((\d+)\): log\.(do_[a-z_]+)\.(\d+)$")
            .unwrap();
    let mut n = 0;
    for l in text.lines() {
        let c = re.captures(l).unwrap_or_else(|| panic!("{l}"));
        assert_eq!(&c[1], &c[3]);
        assert_eq!(&c[2], &c[4]);
        n += 1;
    }
    assert!(n > 18, "{n}");
    assert!(text.starts_with("20261117-164440.117779 do_recipe_qa (1460780)"));
    assert!(
        text.trim_end()
            .ends_with("do_compile (1757933): log.do_compile.1757933")
    );
}

// ---------------------------------------------------------------------------------------
// The bitbake tool: `op=build target=curl` and two cancels.

/// A cancelled build's job output. Killed with SIGTERM (before y1-build's fix) the client
/// printed nothing more: its output stops at `do_configure: Started` while the server built
/// on. Interrupted twice it printed knotty's two messages, the server stopped the running
/// task, and the client ended with `Execution was interrupted`. Neither is a failure the
/// digester should report.
#[test]
fn real_cancelled_build_outputs() {
    let killed = sample("bitbake/job-build-curl-cancel-sigterm.out");
    assert!(
        killed
            .trim_end()
            .ends_with("NOTE: recipe curl-8.19.0-r0: task do_configure: Started")
    );
    let interrupted = sample("bitbake/job-build-curl-cancel-sigint.out");
    let at = |s: &str| interrupted.find(s).unwrap_or_else(|| panic!("{s}"));
    assert!(
        at("Keyboard Interrupt, closing down...") < at("Second Keyboard Interrupt, stopping...")
    );
    assert!(at("Second Keyboard Interrupt") < at("NOTE: Sending SIGTERM to remaining 1 tasks"));
    assert!(
        interrupted
            .trim_end()
            .ends_with("Execution was interrupted, returning a non-zero exit code.")
    );
    for text in [&killed, &interrupted] {
        assert!(logs::detect(text).is_empty());
        assert!(logs::failed_tasks(text).is_empty());
    }
    let done = sample("bitbake/job-build-curl.out");
    assert!(done.contains(
        "NOTE: Tasks Summary: Attempted 919 tasks of which 900 didn't need to be rerun and all succeeded."
    ));
    let daemon = sample("bitbake/cookerdaemon-cancel-excerpt.log");
    assert!(daemon.contains("23:16:35.315404 Disconnecting Client"));
    assert!(daemon.contains("23:16:55.051976 No timeout, exiting."));
    assert!(daemon.contains("23:24:04.076926 Running command ['stateShutdown']"));
    assert!(daemon.contains("23:24:05.077478 Running command ['stateForceShutdown']"));
}

// ---------------------------------------------------------------------------------------
// recipe show=cve: the real per-recipe report of `bitbake -c generate_vex curl`, with
// `inherit vex` in a scratch bbappend (curl only).

use sous_chef::bitbake::cve::{self, CveRules};
use sous_chef::release::Release;

/// The report reads as the view reads it (21 Patched, 7 Ignored), and every Patched issue
/// is the CVE the Wrynose file-name rule takes from its patch files (`CVE-2026-6429.patch`
/// and `CVE-2026-6429-dependent.patch`, `CVE-2026-9545-01.patch`, ...).
#[test]
fn real_vex_report_of_curl() {
    let text = sample("cve/curl_cve.json");
    let p = cve::read_check_json(&text, "curl").unwrap().unwrap();
    assert_eq!((p.name.as_str(), p.version.as_str()), ("curl", "8.19.0"));
    let counts = p.counts();
    assert_eq!(counts.get("Patched"), Some(&21));
    assert_eq!(counts.get("Ignored"), Some(&7));
    assert_eq!(counts.len(), 2);
    for i in &p.issues {
        let want = if i.status == "Patched" {
            "fix-file-included"
        } else {
            "not-applicable-config"
        };
        if i.id != "CVE-2024-32928" {
            assert_eq!(i.detail.as_deref(), Some(want), "{}", i.id);
        } else {
            assert_eq!(i.detail.as_deref(), Some("ignored"));
        }
        assert!(i.scores.is_empty(), "vex writes no NVD fields: {i:?}");
    }
    let rules = CveRules::for_release(Release::Wrynose);
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut files = 0;
    for i in v["package"][0]["issue"].as_array().unwrap() {
        for f in i["patch-file"].as_array().into_iter().flatten() {
            let f = f.as_str().unwrap();
            assert_eq!(
                cve::cve_from_filename(f, rules).as_deref(),
                i["id"].as_str(),
                "{f}"
            );
            files += 1;
        }
    }
    assert_eq!(files, 25);
    assert_eq!(v["package"][0]["cpes"].as_array().unwrap().len(), 6);
}

// ---------------------------------------------------------------------------------------
// Live (ignored): the whole package data of the build directory, when `SOUS_CHEF_LIVE`
// names the live environment. Reads files only; starts nothing.

/// Every `(package, path)` of `find_path *` over the build's whole `PKGDATA_DIR` against
/// the recorded `oe-pkgdata-util find-path '*'` of the same build
/// (`$SOUS_CHEF_LIVE/runs/y1-build/logs/find-path-all.out`, 10505 lines).
#[test]
#[ignore = "needs SOUS_CHEF_LIVE"]
fn live_whole_pkgdata_find_path_equals_oe_pkgdata_util() {
    let Some(root) = std::env::var_os("SOUS_CHEF_LIVE").map(PathBuf::from) else {
        return;
    };
    let dir = root.join("yocto-kiss/build/tmp/pkgdata/dogbonedark");
    let recorded = root.join("runs/y1-build/logs/find-path-all.out");
    if !dir.join("runtime").is_dir() || !recorded.is_file() {
        eprintln!("no package data or no recorded find-path output: skipping");
        return;
    }
    let pd = PkgData::new(dir, &open_dir);
    let ours: BTreeSet<String> = pd
        .find_path(&pkgdata::glob_regex("*").unwrap())
        .into_iter()
        .map(|(p, path)| format!("{}: {}", p.name, as_oe_prints(&path)))
        .collect();
    let theirs: BTreeSet<String> = fs::read_to_string(recorded)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let only_ours: Vec<&String> = ours.difference(&theirs).take(5).collect();
    let only_theirs: Vec<&String> = theirs.difference(&ours).take(5).collect();
    assert!(
        only_ours.is_empty() && only_theirs.is_empty(),
        "ours only {only_ours:?}, theirs only {only_theirs:?}"
    );
}

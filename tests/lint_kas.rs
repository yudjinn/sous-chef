//! T9.3: `org/generated-conf-edit` on its fixtures, in the kas fixture project after
//! checkout, and the kas note of `org/recipe-change-in-local-conf`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod kas_support;

use std::fs;
use std::path::{Path, PathBuf};

use kas_support::Project;
use sous_chef::index::Index;
use sous_chef::lint::rules::org_generated_conf_edit::OrgGeneratedConfEdit;
use sous_chef::lint::rules::org_recipe_change_in_local_conf::OrgRecipeChangeInLocalConf;
use sous_chef::lint::{self, Finding, Options, Rule, Target};
use sous_chef::types::{FileKind, LineSpan, Loc};
use sous_chef::workspace::Workspace;

struct Env {
    p: Project,
    ws: Workspace,
    index: Index,
    _cache: tempfile::TempDir,
}

fn env(p: Project) -> Env {
    let ws = p.ws();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    Env {
        p,
        ws,
        index,
        _cache: cache,
    }
}

fn check(e: &Env, rule: &dyn Rule, fixture: &str, path: &str) -> Vec<Finding> {
    let text = fs::read_to_string(common::fixture_path(&format!(
        "lint/org/generated-conf-edit/{fixture}"
    )))
    .unwrap();
    let loc = Loc {
        layer: "build".into(),
        path: PathBuf::from(path),
        lines: LineSpan::line(1),
    };
    lint::check_text(
        rule,
        &e.ws,
        &e.index,
        loc,
        &text,
        FileKind::from_path(Path::new(path)),
        &[],
        None,
    )
}

fn lines_and_messages(found: &[Finding]) -> Vec<(String, String)> {
    found
        .iter()
        .map(|f| (f.loc.lines.to_string(), f.message.clone()))
        .collect()
}

#[test]
fn generated_conf_edit_local_conf() {
    let e = env(Project::after());
    let found = check(&e, &OrgGeneratedConfEdit, "bad.conf", "conf/local.conf");
    let tail = "comes from no kas file, and kas rewrites local.conf on every checkout, shell and build, so it will be lost; add it to local_conf_header in kas/common.yaml";
    assert_eq!(
        lines_and_messages(&found),
        [
            (
                "4".into(),
                format!("`BB_NUMBER_THREADS` in local.conf {tail}")
            ),
            ("8".into(), format!("`IMAGE_FSTYPES` in local.conf {tail}")),
            ("13".into(), format!("`INHERIT` in local.conf {tail}")),
        ]
    );
    assert!(found.iter().all(|f| f.severity == lint::Severity::Warning));
    assert!(check(&e, &OrgGeneratedConfEdit, "good.conf", "conf/local.conf").is_empty());
}

#[test]
fn generated_conf_edit_bblayers_conf() {
    let e = env(Project::after());
    let found = check(
        &e,
        &OrgGeneratedConfEdit,
        "bad-bblayers.conf",
        "conf/bblayers.conf",
    );
    assert_eq!(
        lines_and_messages(&found),
        [
            (
                "8".into(),
                "layer ${TOPDIR}/../meta-local in BBLAYERS is in no kas file, and kas rewrites bblayers.conf on every checkout, shell and build, so it will be dropped; add it under repos in kas/common.yaml".into()
            ),
            (
                "13".into(),
                "`BBLAYERS` in bblayers.conf comes from no kas file, and kas rewrites bblayers.conf on every checkout, shell and build, so it will be lost; add it to bblayers_conf_header in kas/common.yaml".into()
            ),
        ]
    );
    assert!(
        check(
            &e,
            &OrgGeneratedConfEdit,
            "good-bblayers.conf",
            "conf/bblayers.conf"
        )
        .is_empty()
    );
}

#[test]
fn generated_conf_edit_is_silent_when_it_cannot_tell() {
    // Outside a kas project.
    let ws = common::fixture_ws();
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    let text = fs::read_to_string(common::fixture_path(
        "lint/org/generated-conf-edit/bad.conf",
    ))
    .unwrap();
    let loc = Loc {
        layer: "build".into(),
        path: PathBuf::from("conf/local.conf"),
        lines: LineSpan::line(1),
    };
    assert!(
        lint::check_text(
            &OrgGeneratedConfEdit,
            &ws,
            &index,
            loc,
            &text,
            Some(FileKind::Conf),
            &[],
            None
        )
        .is_empty()
    );

    // A kas file that could not be read fully (an include of a repo not checked out).
    let p = Project::after();
    let base = p.path("kas/base.yaml");
    let t = fs::read_to_string(&base).unwrap().replace(
        "    - kas/common.yaml\n",
        "    - kas/common.yaml\n    - repo: bitbake\n      file: kas/x.yaml\n",
    );
    fs::write(&base, t).unwrap();
    let e = env(p);
    assert!(!e.ws.kas.as_ref().unwrap().problems.is_empty());
    assert!(check(&e, &OrgGeneratedConfEdit, "bad.conf", "conf/local.conf").is_empty());

    // Not kas-generated: no CACHEDIR.TAG and none of kas's own lines.
    let p = Project::after();
    fs::remove_file(p.path("build/CACHEDIR.TAG")).unwrap();
    let e = env(p);
    let found = check(&e, &OrgGeneratedConfEdit, "bad.conf", "conf/local.conf");
    assert_eq!(found.len(), 3, "kas's own lines are enough");
    let text = "INHERIT += \"rm_work\"\n";
    let loc = Loc {
        layer: "build".into(),
        path: PathBuf::from("conf/local.conf"),
        lines: LineSpan::line(1),
    };
    assert!(
        lint::check_text(
            &OrgGeneratedConfEdit,
            &e.ws,
            &e.index,
            loc,
            text,
            Some(FileKind::Conf),
            &[],
            None
        )
        .is_empty()
    );
}

/// Through the engine: linting the kas workspace reports the hand edit in local.conf.
#[test]
fn whole_workspace_lint_reports_the_edit() {
    let p = Project::after();
    let conf = p.path("build/conf/local.conf");
    let t = fs::read_to_string(&conf).unwrap() + "BB_NUMBER_THREADS = \"4\"\n";
    fs::write(&conf, t).unwrap();
    let e = env(p);
    let mut cfg = e.p.config();
    cfg.bitbake.enabled = false;
    let report = lint::run(
        &e.ws,
        &e.index,
        &cfg,
        &Target::Workspace,
        &Options {
            rules: vec!["org/generated-conf-edit".into()],
            ..Options::default()
        },
    )
    .unwrap();
    let got: Vec<String> = report.findings.iter().map(|f| f.loc.to_string()).collect();
    assert_eq!(got, ["build:conf/local.conf:12"]);
}

#[test]
fn recipe_change_in_local_conf_points_at_the_kas_file() {
    let e = env(Project::after());
    let text = "EXTRA_OECONF:append:pn-app = \" --x\"\n";
    let loc = Loc {
        layer: "build".into(),
        path: PathBuf::from("conf/local.conf"),
        lines: LineSpan::line(1),
    };
    let found = lint::check_text(
        &OrgRecipeChangeInLocalConf,
        &e.ws,
        &e.index,
        loc.clone(),
        text,
        Some(FileKind::Conf),
        &[],
        None,
    );
    assert_eq!(found.len(), 1);
    assert!(
        found[0].message.ends_with(
            "; kas rewrites this local.conf on its next run, so if it must stay a local setting put it in local_conf_header in kas/common.yaml"
        ),
        "{}",
        found[0].message
    );

    // A line kas wrote from a header entry: the note names the entry.
    let p = Project::after();
    let common = p.path("kas/common.yaml");
    let t = fs::read_to_string(&common).unwrap().replace(
        "    CONF_VERSION = \"2\"\n",
        "    CONF_VERSION = \"2\"\n    EXTRA_OECONF:append:pn-app = \" --x\"\n",
    );
    fs::write(&common, t).unwrap();
    let e = env(p);
    let found = lint::check_text(
        &OrgRecipeChangeInLocalConf,
        &e.ws,
        &e.index,
        loc,
        text,
        Some(FileKind::Conf),
        &[],
        None,
    );
    assert!(
        found[0].message.ends_with(
            "; kas generated it from local_conf_header entry `common` at kas/common.yaml:7, so change it there"
        ),
        "{}",
        found[0].message
    );
}

/// When the kas work directory can only be guessed (the build directory has kas's
/// `bblayers.conf` lines but neither kas's `CACHEDIR.TAG` nor kas's `local.conf`, and
/// sous-chef.toml is elsewhere), where the remote repositories are is a guess too: no
/// `BBLAYERS` entry is judged, while a statement kas does not write still is.
#[test]
fn generated_conf_edit_does_not_judge_bblayers_under_a_guessed_work_dir() {
    let p = Project::after();
    fs::remove_file(p.path("build/CACHEDIR.TAG")).unwrap();
    fs::write(p.path("build/conf/local.conf"), "CONF_VERSION = \"2\"\n").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let cfg = sous_chef::config::Config {
        build_dir: Some(p.path("build")),
        work_dir: elsewhere.path().to_path_buf(),
        config_dir: Some(elsewhere.path().to_path_buf()),
        ..p.config()
    };
    let ws = Workspace::discover_in(&cfg, &sous_chef::workspace::kas::KasEnv::default()).unwrap();
    assert!(
        ws.kas
            .as_ref()
            .is_some_and(|k| k.work_dir_source.is_guess())
    );
    let cache = tempfile::tempdir().unwrap();
    let mut index = Index::open_in(cache.path(), &ws).unwrap();
    index.refresh(&ws).unwrap();
    let e = Env {
        p,
        ws,
        index,
        _cache: cache,
    };
    let found = check(
        &e,
        &OrgGeneratedConfEdit,
        "bad-bblayers.conf",
        "conf/bblayers.conf",
    );
    let lines: Vec<String> = found.iter().map(|f| f.loc.lines.to_string()).collect();
    assert_eq!(lines, ["13"]);
}

/// The CLI run from a directory outside the kas project, with a configuration file in a
/// third directory naming the kas files and the kas build directory: the kas work directory
/// is the build directory's parent (kas's default build directory is `<work dir>/build`),
/// not the directory sous-chef runs in, so the downloaded repositories' layers in
/// `BBLAYERS` are kas layers and nothing is reported.
#[test]
fn generated_conf_edit_does_not_depend_on_the_current_directory() {
    let p = Project::after();
    let elsewhere = tempfile::tempdir().unwrap();
    let etc = elsewhere.path().join("etc");
    let cwd = elsewhere.path().join("cwd");
    fs::create_dir_all(&etc).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    fs::write(
        etc.join("sous-chef.toml"),
        format!(
            "build_dir = {:?}\nkas = [{:?}, {:?}]\n[bitbake]\nenabled = false\n",
            p.path("build").display().to_string(),
            p.path("kas/base.yaml").display().to_string(),
            p.path("kas/board.yaml").display().to_string(),
        ),
    )
    .unwrap();
    for dir in [cwd.as_path(), p.root.as_path(), etc.as_path()] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_sous-chef"))
            .arg("--config")
            .arg(etc.join("sous-chef.toml"))
            .args(["lint", "--rule", "org/generated-conf-edit", "build"])
            .env("XDG_CACHE_HOME", elsewhere.path().join("cache"))
            .env_remove("KAS_WORK_DIR")
            .env_remove("KAS_BUILD_DIR")
            .current_dir(dir)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            out.status.code(),
            Some(0),
            "from {}: {out:?}",
            dir.display()
        );
        assert!(
            stdout.starts_with("no findings"),
            "from {}: {stdout}",
            dir.display()
        );
    }
}

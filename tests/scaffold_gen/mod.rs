//! Shared harness for the generator tests (T6.2, T6.3): a temporary copy of the fixture
//! workspace, the real generators through `scaffold::execute`, and the checks every
//! generated file must pass.
#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use sous_chef::config::Config;
use sous_chef::error::Result;
use sous_chef::index::Index;
use sous_chef::lint::Severity;
use sous_chef::parse::{StatementKind, parse};
use sous_chef::scaffold::{self, Kind, Outcome, Request, RuleLint};
use sous_chef::workspace::Workspace;

use crate::common;

pub struct Env {
    _tmp: tempfile::TempDir,
    _cache: tempfile::TempDir,
    /// Canonical workspace root of the copy.
    pub root: PathBuf,
    pub cfg: Config,
    pub ws: Workspace,
    pub index: Index,
}

impl Env {
    /// The fixture workspace (Wrynose; `meta-mine` writable, claiming kirkstone scarthgap
    /// wrynose).
    pub fn new() -> Env {
        Env::edited(|_| {})
    }

    /// The fixture workspace with `edit(root)` applied before discovery.
    pub fn edited(edit: impl FnOnce(&Path)) -> Env {
        let (tmp, root) = common::temp_ws();
        let root = fs::canonicalize(root).unwrap();
        edit(&root);
        let cfg = common::ws_config(&root);
        Env::finish(tmp, root, cfg)
    }

    /// The fixture workspace with `release = "<codename>"` in `sous-chef.toml`.
    pub fn release(codename: &str, edit: impl FnOnce(&Path)) -> Env {
        let (tmp, root, cfg, _ws) = common::temp_ws_release(codename, edit);
        Env::finish(tmp, root, cfg)
    }

    fn finish(tmp: tempfile::TempDir, root: PathBuf, cfg: Config) -> Env {
        let ws = Workspace::discover(&cfg).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let index = Index::open_in(cache.path(), &ws).unwrap();
        Env {
            _tmp: tmp,
            _cache: cache,
            root,
            cfg,
            ws,
            index,
        }
    }

    /// Discovers the workspace again (after a new layer was added to `bblayers.conf`).
    pub fn rediscover(&mut self) {
        self.ws = Workspace::discover(&self.cfg).unwrap();
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn run(&mut self, req: &Request, write: bool) -> Result<Outcome> {
        scaffold::execute(
            &self.ws,
            &self.cfg,
            &mut self.index,
            req,
            write,
            None,
            &RuleLint,
        )
    }

    /// Dry run; panics on error.
    pub fn plan(&mut self, req: &Request) -> Outcome {
        self.run(req, false).unwrap()
    }

    /// Dry run, rendered as [`Env::render`] does.
    pub fn show(&mut self, req: &Request) -> String {
        let o = self.plan(req);
        self.render(&o)
    }

    /// Writes the plan and asserts the post-write lint found nothing at warning or above.
    pub fn write_clean(&mut self, req: &Request) -> Outcome {
        let o = self.run(req, true).unwrap();
        assert!(o.written);
        assert_lint_clean(&o);
        o
    }

    /// The outcome as snapshot text: each file (action, display path, content), then the
    /// notes, with the temporary root replaced by `<ws>`.
    pub fn render(&self, o: &Outcome) -> String {
        let mut out = String::new();
        for c in &o.checked {
            let f = &o.plan.files[c.index];
            out.push_str(&format!("--- {} {}\n", f.action.as_str(), c.display));
            out.push_str(&f.content);
        }
        for n in &o.plan.notes {
            out.push_str(&format!("note: {n}\n"));
        }
        out.replace(&self.root.display().to_string(), "<ws>")
    }
}

pub fn request(
    kind: Kind,
    name: &str,
    version: Option<&str>,
    options: serde_json::Value,
) -> Request {
    Request {
        kind,
        layer: None,
        name: Some(name.to_string()),
        version: version.map(str::to_string),
        options: options.as_object().cloned().unwrap_or_default(),
    }
}

/// Every statement is known and the parser reports nothing.
pub fn assert_parses(what: &str, content: &str) {
    let parsed = parse(content);
    for s in &parsed.statements {
        assert!(
            !matches!(s.kind, StatementKind::Unknown { .. }),
            "{what}: unknown statement {:?}\n{content}",
            s.kind
        );
    }
    assert!(
        parsed.diagnostics.is_empty(),
        "{what}: {:?}\n{content}",
        parsed.diagnostics
    );
}

/// Every planned file parses cleanly.
pub fn assert_plan_parses(o: &Outcome) {
    for f in &o.plan.files {
        let metadata = f.path.extension().is_some_and(|e| {
            ["bb", "bbappend", "inc", "bbclass", "conf"].contains(&e.to_string_lossy().as_ref())
        });
        if !metadata {
            continue;
        }
        assert_parses(&f.path.display().to_string(), &f.content);
    }
}

/// The post-write lint ran and found nothing at warning level or above.
pub fn assert_lint_clean(o: &Outcome) {
    let findings = match &o.lint {
        Some(Ok(f)) => f,
        other => panic!("post-write lint did not run cleanly: {other:?}"),
    };
    let bad: Vec<String> = findings
        .iter()
        .filter(|f| f.severity >= Severity::Warning)
        .map(|f| format!("{} {} {} {}", f.severity.as_str(), f.rule, f.loc, f.message))
        .collect();
    assert!(
        bad.is_empty(),
        "lint findings on generated files:\n{}",
        bad.join("\n")
    );
}

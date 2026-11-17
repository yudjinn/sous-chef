//! Helpers for the kas tests (T9.3): copies of the kas project fixture
//! (`tests/fixtures/kas/`) before and after a (simulated) `kas checkout`.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use sous_chef::config::Config;
use sous_chef::workspace::Workspace;
use sous_chef::workspace::kas::KasEnv;

use super::common;

/// A temporary copy of the kas project. `root` is the project (the repository kas files
/// live in; a `.git` directory is created so it is a repository root, as yocto-kiss is).
pub struct Project {
    pub _tmp: tempfile::TempDir,
    pub root: PathBuf,
}

impl Project {
    /// The project before `kas checkout`: only its own repository.
    pub fn before() -> Project {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        common::copy_dir(&common::fixture_path("kas/project"), &root);
        fs::create_dir_all(root.join(".git")).unwrap();
        let root = root.canonicalize().unwrap();
        Project { _tmp: tmp, root }
    }

    /// The project after `kas checkout kas/base.yaml:kas/board.yaml`: the downloaded
    /// repositories next to the project's own layers, and `build/` as kas writes it.
    pub fn after() -> Project {
        let p = Project::before();
        common::copy_dir(&common::fixture_path("kas/checkout"), &p.root);
        p
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// `kas = ["kas/base.yaml", "kas/board.yaml"]`, discovery from the project root,
    /// bitbake enabled.
    pub fn config(&self) -> Config {
        Config {
            kas: vec![self.path("kas/base.yaml"), self.path("kas/board.yaml")],
            work_dir: self.root.clone(),
            ..Config::default()
        }
    }

    /// The workspace discovered with [`Project::config`] and no kas environment.
    pub fn ws(&self) -> Workspace {
        Workspace::discover_in(&self.config(), &KasEnv::default()).unwrap()
    }
}

/// Names of the workspace layers, in order.
pub fn names(ws: &Workspace) -> Vec<String> {
    ws.layers.iter().map(|l| l.name.clone()).collect()
}

/// `path` relative to `root`, as a string.
pub fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

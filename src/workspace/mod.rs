//! Workspace and layers. Contract: `docs/architecture.md` ("Workspace contract").

pub mod kas;
pub mod layers;
pub mod resolve;

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::release::{self, LintReach, Release, ReleaseInfo};
use crate::types::{LineSpan, Loc};

/// The pseudo-layer name `loc_for` uses for files in the build directory.
pub const BUILD_LAYER: &str = "build";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Workspace {
    pub build_dir: Option<PathBuf>,
    /// In `BBLAYERS` order.
    pub layers: Vec<Layer>,
    /// The Yocto release the workspace targets (see [`release::detect`]). The default is
    /// [`Release::DEFAULT`], assumed.
    pub release: ReleaseInfo,
    /// The kas project, when this is one ([`kas::detect`]); `None` otherwise.
    pub kas: Option<kas::KasProject>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Layer {
    /// Directory name, unique within the workspace.
    pub name: String,
    /// Absolute.
    pub root: PathBuf,
    /// `BBFILE_COLLECTIONS`
    pub collections: Vec<String>,
    /// `BBFILE_PRIORITY`
    pub priority: i32,
    /// `LAYERSERIES_COMPAT`
    pub compat: Vec<String>,
    /// `LAYERDEPENDS`
    pub depends: Vec<String>,
    /// `BBFILES` globs, `${LAYERDIR}` substituted.
    pub bbfiles: Vec<String>,
    /// From `sous-chef.toml` `writable_layers`.
    pub writable: bool,
}

impl Workspace {
    /// Finds the build directory and layers (see "Configuration" in architecture.md).
    ///
    /// 1. `cfg.build_dir`, else the nearest ancestor of `cfg.work_dir` with
    ///    `conf/bblayers.conf`: layers are `BBLAYERS`, in order.
    /// 2. No build directory: `cfg.layers`, else every directory up to
    ///    [`layers::SCAN_DEPTH`] levels below `cfg.work_dir` with `conf/layer.conf`.
    ///
    /// Layer roots and the build directory are stored canonicalised (symlinks resolved).
    /// `BBLAYERS` entries that cannot be expanded statically are skipped with a warning on
    /// stderr; use [`layers::read_bblayers`] to inspect them.
    ///
    /// The release is detected last, once the layers are known and marked writable:
    /// `cfg.release`, then `LAYERSERIES_CORENAMES`, then the layers' `LAYERSERIES_COMPAT`
    /// ([`release::detect`]). An unknown `cfg.release` is an error.
    ///
    /// A kas project ([`kas::detect`], reading `KAS_WORK_DIR` and `KAS_BUILD_DIR` of this
    /// process) changes three things, and nothing when there is none: its build directory
    /// is used when the walk finds none; without a build directory its present layers (in
    /// `BBLAYERS` order as kas writes it) are the layers, its missing ones only listed; and
    /// with `writable_layers` empty, its own layers are writable. With a build directory,
    /// `bblayers.conf` stays the authority for the layer list and the differences are
    /// recorded in [`kas::KasProject::not_in_bblayers`] and `not_in_kas`.
    pub fn discover(cfg: &Config) -> Result<Workspace> {
        Workspace::discover_in(cfg, &kas::KasEnv::from_process())
    }

    /// [`Workspace::discover`] with an explicit kas environment.
    pub fn discover_in(cfg: &Config, kas_env: &kas::KasEnv) -> Result<Workspace> {
        let work_dir = absolute(&cfg.work_dir)?;
        let mut kas = kas::detect(cfg, kas_env)?;
        let build_dir = match &cfg.build_dir {
            Some(b) => {
                let b = work_dir.join(b);
                if !b.join("conf").join("bblayers.conf").is_file() {
                    return Err(Error::Config {
                        msg: format!(
                            "build directory {} has no conf/bblayers.conf; fix build_dir or --build-dir",
                            b.display()
                        ),
                    });
                }
                Some(b)
            }
            None => layers::find_build_dir(&work_dir).or_else(|| {
                kas.as_ref()
                    .map(|k| k.build_dir.clone())
                    .filter(|b| b.join("conf").join("bblayers.conf").is_file())
            }),
        };

        let (build_dir, (mut found, corenames)) = match build_dir {
            Some(b) => {
                let b = canonical(&b, "build directory")?;
                let bb = layers::read_bblayers(&b)?;
                for u in &bb.unresolved {
                    eprintln!(
                        "sous-chef: warning: BBLAYERS entry {u:?} in {} cannot be expanded statically; skipped",
                        b.join("conf/bblayers.conf").display()
                    );
                }
                if bb.layers.is_empty() {
                    return Err(Error::Config {
                        msg: format!(
                            "BBLAYERS in {} lists no layer that can be resolved",
                            b.join("conf/bblayers.conf").display()
                        ),
                    });
                }
                let mut roots = Vec::with_capacity(bb.layers.len());
                for l in &bb.layers {
                    // BitBake checks relative entries against its working directory.
                    let abs = work_dir.join(l);
                    if !abs.is_dir() {
                        // kas-container writes `${TOPDIR}/<relative path>` computed inside
                        // the container (the kas files' repository at `/repo`, the work
                        // directory at `/work`), which can name nothing on the host
                        // (docs/bitbake-semantics.md, "kas").
                        let hint = if kas.as_ref().is_some_and(|k| k.generates(&b)) {
                            "; if kas-container wrote this bblayers.conf, its paths are the container's (the kas files' repository is /repo there): run kas checkout outside the container to write host paths"
                        } else {
                            ""
                        };
                        return Err(Error::Config {
                            msg: format!(
                                "BBLAYERS in {} lists {}, which is not a directory{hint}",
                                b.join("conf/bblayers.conf").display(),
                                abs.display()
                            ),
                        });
                    }
                    roots.push(canonical(&abs, "layer")?);
                }
                let found = layers::read_layer_confs_with_corenames(&roots, Some(&bb))?;
                (Some(b), found)
            }
            None => {
                let roots = match kas.as_ref().filter(|k| !k.files.is_empty()) {
                    Some(k) => {
                        let mut roots: Vec<PathBuf> = k
                            .layers
                            .iter()
                            .filter(|l| l.present)
                            .map(|l| l.path.clone())
                            .collect();
                        for l in &cfg.layers {
                            let l = work_dir.join(l);
                            if !roots.iter().any(|r| kas::same_path(r, &l)) {
                                roots.push(l);
                            }
                        }
                        roots
                    }
                    None if cfg.layers.is_empty() => {
                        layers::scan_for_layers(&work_dir, layers::SCAN_DEPTH)
                    }
                    None => cfg.layers.iter().map(|l| work_dir.join(l)).collect(),
                };
                let mut canon = Vec::with_capacity(roots.len());
                for r in &roots {
                    let c = canonical(r, "layer")?;
                    if !c.join("conf").join("layer.conf").is_file() {
                        return Err(Error::Config {
                            msg: format!("layer {} has no conf/layer.conf", c.display()),
                        });
                    }
                    canon.push(c);
                }
                (None, layers::read_layer_confs_with_corenames(&canon, None)?)
            }
        };

        assign_names(&mut found);
        for l in &mut found {
            l.writable = cfg.writable_layers.contains(&l.name);
        }
        if let Some(k) = kas.as_mut() {
            if cfg.writable_layers.is_empty() {
                for l in &mut found {
                    l.writable = k.own_layer(&l.root).is_some();
                }
            }
            if let Some(b) = &build_dir {
                if k.generates(b) {
                    k.not_in_bblayers = k
                        .layers
                        .iter()
                        .filter(|kl| !found.iter().any(|l| kas::same_path(&l.root, &kl.path)))
                        .map(|kl| kl.path.clone())
                        .collect();
                    k.not_in_kas = found
                        .iter()
                        .filter(|l| k.layer_at(&l.root).is_none())
                        .map(|l| l.name.clone())
                        .collect();
                } else if !k.files.is_empty() {
                    k.problems.push(format!(
                        "the build directory {} is not the kas build directory {}; its configuration is not generated from the kas files",
                        b.display(),
                        k.build_dir.display()
                    ));
                }
            }
        }
        let release = release::detect(cfg.release.as_deref(), &corenames, &found)?;
        Ok(Workspace {
            build_dir,
            layers: found,
            release,
            kas,
        })
    }

    /// The pointer for an absolute path inside a layer (or the build directory, as the
    /// pseudo-layer `build`). The innermost containing layer wins.
    pub fn loc_for(&self, abs: &Path, lines: LineSpan) -> Option<Loc> {
        let lexical = normalize(abs);
        self.loc_for_normalized(&lexical, lines).or_else(|| {
            let canon = fs::canonicalize(abs).ok()?;
            self.loc_for_normalized(&canon, lines)
        })
    }

    fn loc_for_normalized(&self, abs: &Path, lines: LineSpan) -> Option<Loc> {
        let best = self
            .layers
            .iter()
            .filter_map(|l| abs.strip_prefix(&l.root).ok().map(|rel| (l, rel)))
            .max_by_key(|(l, _)| l.root.components().count());
        if let Some((l, rel)) = best {
            return Some(Loc {
                layer: l.name.clone(),
                path: rel.to_path_buf(),
                lines,
            });
        }
        let rel = abs.strip_prefix(self.build_dir.as_ref()?).ok()?;
        Some(Loc {
            layer: BUILD_LAYER.to_string(),
            path: rel.to_path_buf(),
            lines,
        })
    }

    /// The absolute path a pointer refers to. `None` for an unknown layer or a path that is
    /// absolute or contains `..`.
    pub fn abs_path(&self, loc: &Loc) -> Option<PathBuf> {
        let ok = loc
            .path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        if !ok {
            return None;
        }
        let root = match self.layer(&loc.layer) {
            Some(l) => &l.root,
            None if loc.layer == BUILD_LAYER => self.build_dir.as_ref()?,
            None => return None,
        };
        Some(root.join(&loc.path))
    }

    /// Confinement check: resolves symlinks and requires the result to be inside a layer
    /// root or the build directory. Every file access goes through this.
    ///
    /// The path need not exist (scaffold checks files it is about to create): the longest
    /// existing prefix is resolved through the filesystem and the rest lexically. A relative
    /// path is taken relative to the process working directory. Returns the resolved path.
    pub fn confine(&self, path: &Path) -> Result<PathBuf> {
        let denied = || Error::Confined {
            path: path.to_path_buf(),
        };
        let resolved = resolve_existing(path).map_err(|_| denied())?;
        let inside = self
            .layers
            .iter()
            .map(|l| &l.root)
            .chain(self.build_dir.as_ref())
            .any(|root| resolved.starts_with(root));
        if inside { Ok(resolved) } else { Err(denied()) }
    }

    /// Looks up a layer by name.
    pub fn layer(&self, name: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.name == name)
    }
}

impl Layer {
    /// The releases this layer is linted against when it is the lint target or contains it
    /// ([`release::lint_releases`] with [`LintReach::Target`]): every supported release in
    /// `compat` plus the workspace release, oldest first. This is also what a writable
    /// layer is always checked against, so scaffold asks it.
    pub fn lint_releases(&self, ws: &Workspace) -> Vec<Release> {
        self.lint_releases_as(ws, LintReach::Target)
    }

    /// [`release::lint_releases`] for this layer reached as `reach`: the workspace release
    /// alone only for a layer that is not writable, swept in by a whole-workspace run in a
    /// workspace with a writable layer.
    pub fn lint_releases_as(&self, ws: &Workspace, reach: LintReach) -> Vec<Release> {
        let any_writable = ws.layers.iter().any(|l| l.writable);
        release::lint_releases(self, ws.release.release, any_writable, reach)
    }
}

/// Makes names unique: the directory name, extended with parent directories
/// (`poky/meta`, `other/meta`) for as long as two layers share one.
fn assign_names(layers: &mut [Layer]) {
    let comps: Vec<Vec<String>> = layers
        .iter()
        .map(|l| {
            l.root
                .components()
                .filter_map(|c| match c {
                    Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect()
        })
        .collect();
    let mut depth = vec![1usize; layers.len()];
    let name_of = |i: usize, d: usize| -> String {
        let c = &comps[i];
        let start = c.len().saturating_sub(d);
        c[start..].join("/")
    };
    loop {
        let names: Vec<String> = (0..layers.len()).map(|i| name_of(i, depth[i])).collect();
        let mut changed = false;
        for i in 0..layers.len() {
            let clash = (0..layers.len()).any(|j| j != i && names[j] == names[i]);
            if clash && depth[i] < comps[i].len() {
                depth[i] += 1;
                changed = true;
            }
        }
        if !changed {
            for (i, l) in layers.iter_mut().enumerate() {
                l.name = names[i].clone();
            }
            return;
        }
    }
}

fn absolute(p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(p))
    }
}

fn canonical(p: &Path, what: &str) -> Result<PathBuf> {
    fs::canonicalize(p).map_err(|e| Error::Config {
        msg: format!("{what} {}: {e}", p.display()),
    })
}

/// Lexical normalisation: drops `.`, applies `..` to the preceding component.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Resolves `path` as the OS would: each existing prefix is canonicalised (so symlinks and
/// `..` after a symlink behave as on disk); components past the first missing one are
/// applied lexically.
pub(crate) fn resolve_existing(path: &Path) -> std::io::Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    // The whole path exists (the common case): one `realpath` gives the same result as the
    // walk below, which resolves each prefix again and so costs about depth² lookups.
    if let Ok(real) = fs::canonicalize(&abs) {
        return Ok(real);
    }
    let mut cur = PathBuf::new();
    let mut exists = true;
    for c in abs.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => cur.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                cur.pop();
            }
            Component::Normal(name) => {
                cur.push(name);
                if exists {
                    match fs::canonicalize(&cur) {
                        Ok(real) => cur = real,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            // A dangling symlink is not "missing": refuse it.
                            if fs::symlink_metadata(&cur).is_ok() {
                                return Err(e);
                            }
                            exists = false;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
    }
    Ok(cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(root: &str) -> Layer {
        Layer {
            name: String::new(),
            root: PathBuf::from(root),
            collections: vec![],
            priority: 0,
            compat: vec![],
            depends: vec![],
            bbfiles: vec![],
            writable: false,
        }
    }

    #[test]
    fn names_are_disambiguated_with_parents() {
        let mut ls = vec![
            layer("/src/poky/meta"),
            layer("/src/other/meta"),
            layer("/src/poky/meta-poky"),
            layer("/a/x/meta-y"),
            layer("/b/x/meta-y"),
        ];
        assign_names(&mut ls);
        let names: Vec<_> = ls.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "poky/meta",
                "other/meta",
                "meta-poky",
                "a/x/meta-y",
                "b/x/meta-y"
            ]
        );
    }

    #[test]
    fn normalize_is_lexical() {
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
    }
}

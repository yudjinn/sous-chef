//! Evaluating with some files replaced by in-memory contents ([`Overlay`]).
//!
//! The replay reads a file's statements from the index (`statements` rows) in three places,
//! all in `replay.rs`: `Replay::visit` (the statements of every file parsed), the resolver's
//! set of known files (which `require` / `include` / `inherit` target exists), and
//! `Replay::body_text` (the body of an anonymous python function, read from disk to find the
//! variables it sets). An [`Overlay`] maps absolute paths to parsed contents; wherever one of
//! those three looks a path up, the overlay is consulted first. A path that is not indexed
//! (a file about to be created) becomes a known file. Nothing is written and the index is
//! not touched.
//!
//! The configuration pass is cached per workspace ([`super::ConfigCache`]). An overlay
//! leaves that cache alone: the cached pass is used when no overlay path could have changed
//! it, otherwise the configuration is replayed with the overlay and not cached. "Could have
//! changed it": a `.conf` or `.bbclass` path, a file the cached pass visited, or a path the
//! cached pass probed for an `include` / `require` / `include_all` and did not find (a new
//! file there would have been read). See `replay::evaluate_with_overlay`.
//!
//! The statement rows are built the way `index::scan` builds them for the index (same
//! `kind`, `name`, `base`, `overrides`, `flag`, `op`, `value` columns); `scan.rs` keeps that
//! conversion private, so it is repeated here (`rows`), and the two must change together.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::index::query::StatementRow;
use crate::index::scan::written_name;
use crate::parse::{IncludeMode, Lang, StatementKind, lexer};
use crate::render;
use crate::types::{FileKind, LineSpan, Loc, VarRef};
use crate::workspace::Workspace;

/// Files replaced in memory for one evaluation.
#[derive(Debug, Clone, Default)]
pub struct Overlay {
    files: HashMap<PathBuf, OverlayFile>,
}

/// One replaced (or new) file.
#[derive(Debug, Clone)]
pub(crate) struct OverlayFile {
    pub(crate) layer: String,
    /// Relative to the layer root.
    pub(crate) path: PathBuf,
    pub(crate) kind: FileKind,
    pub(crate) text: String,
    pub(crate) rows: Vec<StatementRow>,
}

impl Overlay {
    pub fn new() -> Overlay {
        Overlay::default()
    }

    /// Replaces (or adds) the file at `abs` with `text`, parsed now. `abs` must be inside a
    /// layer (or the build directory) and have a BitBake file extension.
    pub fn insert(&mut self, ws: &Workspace, abs: &Path, text: &str) -> Result<()> {
        let kind = FileKind::from_path(abs).ok_or_else(|| Error::Invalid {
            msg: format!(
                "{} is not a BitBake file (.bb, .bbappend, .inc, .bbclass, .conf)",
                abs.display()
            ),
        })?;
        let loc = ws
            .loc_for(abs, LineSpan::line(1))
            .ok_or_else(|| Error::Confined {
                path: abs.to_path_buf(),
            })?;
        let key = ws
            .abs_path(&loc)
            .map(|p| super::replay::normalize(&p))
            .ok_or_else(|| Error::Confined {
                path: abs.to_path_buf(),
            })?;
        let rows = rows(text, &loc);
        self.files.insert(
            key,
            OverlayFile {
                layer: loc.layer,
                path: loc.path,
                kind,
                text: text.to_string(),
                rows,
            },
        );
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The absolute paths replaced, in no particular order.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.files.keys().map(PathBuf::as_path)
    }

    /// The text of the lines `loc` covers, when `loc` is in a replaced file (the counterpart
    /// of reading those lines from disk).
    pub fn lines(&self, ws: &Workspace, loc: &Loc) -> Option<String> {
        let abs = super::replay::normalize(&ws.abs_path(loc)?);
        let f = self.files.get(&abs)?;
        Some(
            f.text
                .lines()
                .skip(loc.lines.start.saturating_sub(1) as usize)
                .take((loc.lines.end + 1).saturating_sub(loc.lines.start) as usize)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    pub(crate) fn get(&self, abs: &Path) -> Option<&OverlayFile> {
        self.files.get(abs)
    }

    pub(crate) fn contains(&self, abs: &Path) -> bool {
        self.files.contains_key(abs)
    }
}

/// The statement rows `index::scan` (`file_rows`) would store for `text`.
fn rows(text: &str, at: &Loc) -> Vec<StatementRow> {
    let parsed = crate::parse::parse(text);
    let mut out = Vec::with_capacity(parsed.statements.len());
    for (ord, st) in parsed.statements.iter().enumerate() {
        let src = text.get(st.bytes.clone()).unwrap_or("");
        let mut row = StatementRow {
            id: 0,
            file_id: 0,
            loc: Loc {
                layer: at.layer.clone(),
                path: at.path.clone(),
                lines: LineSpan::new(st.span.start, st.span.end),
            },
            ord: ord as u32,
            kind: String::new(),
            name: None,
            base: None,
            overrides: Vec::new(),
            flag: None,
            op: None,
            legacy: false,
            value: None,
            snippet: render::snippet(src),
        };
        let set_var = |row: &mut StatementRow, var: &VarRef| {
            row.name = Some(written_name(var, src));
            row.base = Some(var.base.clone());
            row.overrides = var.overrides.clone();
            row.flag = var.flag.clone();
            row.legacy = var.legacy_syntax;
        };
        let named = |row: &mut StatementRow, kind: &str, name: &str| {
            row.kind = kind.to_string();
            row.name = Some(name.to_string());
        };
        match &st.kind {
            StatementKind::Assign { var, op, value, .. } => {
                row.kind = "assign".into();
                set_var(&mut row, var);
                row.op = Some(op.as_str().to_string());
                row.value = Some(value.clone());
            }
            StatementKind::Export { name } => {
                named(&mut row, "export", name);
                row.base = Some(name.clone());
            }
            StatementKind::Unset { name, flag } => {
                named(&mut row, "unset", name);
                row.base = Some(name.clone());
                row.flag = flag.clone();
            }
            StatementKind::Function {
                name,
                lang,
                fakeroot,
                ..
            } => {
                row.kind = "function".into();
                match name {
                    Some(v) => set_var(&mut row, v),
                    None => {
                        row.name = Some("__anonymous".into());
                        row.base = Some("__anonymous".into());
                    }
                }
                let l = match lang {
                    Lang::Shell => "shell",
                    Lang::Python => "python",
                };
                row.op = Some(if *fakeroot {
                    format!("fakeroot {l}")
                } else {
                    l.to_string()
                });
            }
            StatementKind::PyDef { name, .. } => {
                named(&mut row, "pydef", name);
                row.base = Some(name.clone());
                row.op = Some("python".into());
            }
            StatementKind::Inherit { classes, deferred } => {
                named(&mut row, "inherit", classes);
                row.op = Some(
                    if *deferred {
                        "inherit_defer"
                    } else {
                        "inherit"
                    }
                    .into(),
                );
            }
            StatementKind::Include { path, mode } => {
                named(&mut row, "include", path);
                row.op = Some(
                    match mode {
                        IncludeMode::Include => "include",
                        IncludeMode::Require => "require",
                        IncludeMode::IncludeAll => "include_all",
                    }
                    .into(),
                );
            }
            StatementKind::AddTask { name, .. } => named(&mut row, "addtask", name),
            StatementKind::DelTask { name } => named(&mut row, "deltask", name),
            StatementKind::AddHandler { name } => named(&mut row, "addhandler", name),
            StatementKind::ExportFunctions { names } => {
                named(&mut row, "export_functions", &names.join(" "))
            }
            StatementKind::AddFragments { rest } => {
                named(
                    &mut row,
                    "addfragments",
                    rest.trim_start_matches(lexer::py_space),
                );
                row.value = Some(format!("addfragments{rest}"));
            }
            StatementKind::AddPyLib { libdir, namespace } => {
                named(&mut row, "addpylib", libdir);
                row.value = Some(namespace.clone());
            }
            StatementKind::Unknown { text } => {
                row.kind = "unknown".into();
                row.value = Some(text.clone());
            }
        }
        out.push(row);
    }
    out
}

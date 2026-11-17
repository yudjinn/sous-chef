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
//! **Limit: the overlay replaces contents, not the collection.** Which recipes and bbappends
//! exist, which bbappends apply to a recipe and which version a recipe name chooses come
//! from the index (its `bbappend` edges, `files.not_collected`, `providers.rs`), never from
//! the overlay: an overlaid `.bbappend` that is not indexed is not applied, a new `.bb` is
//! no recipe, and an overlaid recipe's text does not change the provider choice. The one
//! user, the `.inc` split (`scaffold::inc_split`), never needs more: its plan modifies
//! existing `.bb` files (same names, so the same bbappends match them; BitBake 2.18
//! `cooker.py` `collect_bbfiles` collects only `.bb` and `.bbappend` names and matches a
//! bbappend by basename) and creates one `.inc` (never collected), it evaluates each version
//! by its path (no provider choice), and version data (`PV`, `PE`, `PR`, ...) never moves.
//!
//! The statement rows are built by the scanner's own conversion
//! (`index::scan::statement_rows`), so an overlaid file has exactly the rows the index would
//! store for it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::index::query::StatementRow;
use crate::types::{FileKind, LineSpan, Loc};
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
        let rows = crate::index::scan::statement_rows(text, kind, &loc);
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

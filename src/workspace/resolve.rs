//! `BBPATH` lookup, class lookup and bbappend matching.
//!
//! Stub (M0): T1.4 implements these.

use std::path::{Path, PathBuf};

use super::Workspace;

/// Resolves an `include` / `require` target: absolute, relative to `from`'s directory, then
/// each `BBPATH` entry.
pub fn find_in_bbpath(_ws: &Workspace, _rel: &str, _from: &Path) -> Option<PathBuf> {
    None
}

/// Resolves `inherit name` (`global == false`: `classes-recipe/` then `classes/`) or an
/// `INHERIT` class (`global == true`: `classes-global/` then `classes/`).
pub fn find_class(_ws: &Workspace, _name: &str, _global: bool) -> Option<PathBuf> {
    None
}

/// Whether a bbappend applies to a recipe file (handles the `%` wildcard).
pub fn append_matches(_append_file: &Path, _recipe_file: &Path) -> bool {
    false
}

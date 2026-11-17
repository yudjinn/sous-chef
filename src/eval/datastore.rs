//! Variable store with full operation history.
//!
//! Stub (M0): T3.1 replaces the representation and implements `history`, `files`, `fork`.
//! `value` and `overrides` are implemented by T3.3 in `overrides.rs`.

use serde::Serialize;

use crate::types::{AssignOp, Loc, VarRef};

use super::FileVisit;

/// One recorded operation on a variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VarOp {
    pub var: VarRef,
    pub op: AssignOp,
    pub value: String,
    pub loc: Loc,
    /// The include / inherit / bbappend statements that led here, outermost first.
    pub via: Vec<Loc>,
}

/// What finalisation did with an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum OpStatus {
    Applied,
    InactiveOverride,
    Shadowed,
    Removed,
}

/// The result of an evaluation: every operation on every variable, plus the files visited.
#[derive(Debug, Clone, Default)]
pub struct DataStore {
    pub(crate) visits: Vec<FileVisit>,
}

impl DataStore {
    /// Operations on `base` and all of its override variants, in parse order, with status.
    pub fn history(&self, _base: &str) -> Vec<(VarOp, OpStatus)> {
        Vec::new()
    }

    /// Files in replay order, for `recipe show=tree`.
    pub fn files(&self) -> &[FileVisit] {
        &self.visits
    }
}

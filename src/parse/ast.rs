//! Parser output types. See the parser contract in `docs/architecture.md`.

use serde::Serialize;

use crate::types::{AssignOp, LineSpan, VarRef};

/// Result of parsing one file. Parsing never fails: a bad line becomes
/// [`StatementKind::Unknown`] plus a diagnostic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ParsedFile {
    pub statements: Vec<Statement>,
    pub diagnostics: Vec<ParseDiagnostic>,
}

/// A problem found while parsing (unterminated function, bad continuation, unknown line...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParseDiagnostic {
    pub span: LineSpan,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Statement {
    pub kind: StatementKind,
    /// The whole statement, including continuation lines and function body.
    pub span: LineSpan,
    /// Byte range of the whole statement in the source.
    pub bytes: std::ops::Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum StatementKind {
    /// `value` is the raw text between the quotes with continuation lines joined; no
    /// expansion.
    Assign {
        var: VarRef,
        op: AssignOp,
        value: String,
        export: bool,
    },
    /// Bare `export VAR`.
    Export {
        name: String,
    },
    /// `unset VAR` / `unset VAR[flag]`.
    Unset {
        name: String,
        flag: Option<String>,
    },
    /// Shell or python function. `name == None` is anonymous python.
    Function {
        name: Option<VarRef>,
        lang: Lang,
        fakeroot: bool,
        body: LineSpan,
    },
    /// Top-level python `def`.
    PyDef {
        name: String,
        body: LineSpan,
    },
    /// `inherit` / `inherit_defer`; `classes` is the unexpanded text and may contain `${VAR}`.
    Inherit {
        classes: String,
        deferred: bool,
    },
    /// `include` / `require` / `include_all`.
    Include {
        path: String,
        mode: IncludeMode,
    },
    AddTask {
        name: String,
        before: Vec<String>,
        after: Vec<String>,
    },
    DelTask {
        name: String,
    },
    AddHandler {
        name: String,
    },
    ExportFunctions {
        names: Vec<String>,
    },
    /// Anything the parser did not recognise.
    Unknown {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Lang {
    Shell,
    Python,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum IncludeMode {
    /// `include`: silently ignored if not found.
    Include,
    /// `require`: error if not found.
    Require,
    /// `include_all`: every match on `BBPATH`.
    IncludeAll,
}

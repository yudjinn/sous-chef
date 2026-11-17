//! BitBake parser. Contract: `docs/architecture.md` ("Parser contract").

pub mod ast;
pub mod lexer;
pub mod parser;

pub use ast::{IncludeMode, Lang, ParseDiagnostic, ParsedFile, Statement, StatementKind};

use crate::types::LineSpan;

/// Parses BitBake source. Never fails and never panics.
// Stub (M0): T1.2 implements it.
pub fn parse(src: &str) -> ParsedFile {
    let lines = u32::try_from(src.lines().count().max(1)).unwrap_or(u32::MAX);
    ParsedFile {
        statements: Vec::new(),
        diagnostics: vec![ParseDiagnostic {
            span: LineSpan::new(1, lines),
            message: "parser not implemented".into(),
        }],
    }
}

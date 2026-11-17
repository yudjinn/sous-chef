//! BitBake parser. Contract: `docs/architecture.md` ("Parser contract").

pub mod ast;
pub mod lexer;
pub mod parser;

pub use ast::{IncludeMode, Lang, ParseDiagnostic, ParsedFile, Statement, StatementKind};
pub use parser::split_var;

/// Parses BitBake source. Never fails and never panics.
pub fn parse(src: &str) -> ParsedFile {
    parser::parse(src)
}

/// Parses raw file bytes: invalid UTF-8 is replaced with U+FFFD first (see
/// [`lexer::decode`]), so byte ranges in the result refer to the decoded text, which equals
/// the file whenever it is valid UTF-8.
pub fn parse_bytes(bytes: &[u8]) -> ParsedFile {
    parse(&lexer::decode(bytes))
}

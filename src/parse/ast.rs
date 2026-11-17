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
    /// `addfragments <args>` (OE configuration fragments; BitBake 2.12 and later), with at
    /// least three arguments. `rest` is everything after the keyword, the separating
    /// whitespace included, unexpanded; [`AddFragments::groups`] splits it the way the
    /// release's regular expression does (three groups in 2.12, four from 2.16). A line with
    /// fewer arguments matches no release and is [`StatementKind::Unknown`].
    AddFragments {
        rest: String,
    },
    /// `addpylib <libdir> <namespace>` (BitBake 2.4 and later): the two capture groups of
    /// `ConfHandler`'s `addpylib\s+(.+)\s+(.+)`, unexpanded, as Python's greedy match splits
    /// them (with more than two words, `libdir` takes the extra ones). BitBake accepts it
    /// only while parsing the base configuration (`bblayers.conf`, each `layer.conf`,
    /// `bitbake.conf`; not a file they include) and raises "unparsed line" anywhere else;
    /// the parser records it wherever it appears and `hygiene/parse-error` judges the place.
    /// A line with fewer than two arguments is [`StatementKind::Unknown`].
    AddPyLib {
        libdir: String,
        namespace: String,
    },
    /// Anything the parser did not recognise.
    Unknown {
        text: String,
    },
}

/// The `addfragments` directive (OE configuration fragments; BitBake 2.12 and later). See
/// docs/bitbake-semantics.md, "Configuration fragments".
///
/// The parser makes the line a [`StatementKind::AddFragments`] (no diagnostic);
/// [`Statement::add_fragments`] reads it as this type. How the argument splits depends on
/// the release (three capture groups in BitBake 2.12, four from 2.16), so the text is kept
/// whole and [`AddFragments::groups`] applies either regular expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddFragments {
    /// Everything after the keyword, the separating whitespace included.
    pub rest: String,
}

impl AddFragments {
    /// The capture groups of BitBake's `__addfragments_regexp__` with `n` groups,
    /// `addfragments\s+(.+)\s+(.+)...` (`re.match`, so not anchored at the end), with
    /// Python's backtracking: every `\s+` and `(.+)` greedy, earlier ones first. `None` when
    /// the line does not match (BitBake: "unparsed line"). The groups are unexpanded text;
    /// with more words than groups the first group takes the extra ones
    /// (`a b c d` with three groups is `a b`, `c`, `d`).
    pub fn groups(&self, n: usize) -> Option<Vec<String>> {
        let chars: Vec<char> = self.rest.chars().collect();
        let spans = match_groups(&chars, 0, n)?;
        Some(
            spans
                .into_iter()
                .map(|(a, b)| chars[a..b].iter().collect())
                .collect(),
        )
    }
}

/// `(\s+(.+)){n}` at `pos`, greedy, leftmost first. `.` matches anything but `\n`.
fn match_groups(s: &[char], pos: usize, n: usize) -> Option<Vec<(usize, usize)>> {
    if n == 0 {
        return Some(Vec::new());
    }
    let ws_end = pos
        + s[pos.min(s.len())..]
            .iter()
            .take_while(|c| crate::parse::lexer::py_space(**c))
            .count();
    if ws_end == pos {
        return None;
    }
    for start in (pos + 1..=ws_end).rev() {
        // `.+` from `start`: as far as the first newline, then shorter.
        let max_end = start + s[start..].iter().take_while(|c| **c != '\n').count();
        for end in (start + 1..=max_end).rev() {
            if let Some(mut rest) = match_groups(s, end, n - 1) {
                rest.insert(0, (start, end));
                return Some(rest);
            }
        }
    }
    None
}

impl Statement {
    /// The `addfragments` directive this statement is, if it is one
    /// ([`StatementKind::AddFragments`]). Whether its arguments match the release's regular
    /// expression is [`AddFragments::groups`]'s answer.
    pub fn add_fragments(&self) -> Option<AddFragments> {
        let StatementKind::AddFragments { rest } = &self.kind else {
            return None;
        };
        Some(AddFragments { rest: rest.clone() })
    }
}

/// For a logical line starting with `addpylib` and whitespace: `Some(Some((libdir,
/// namespace)))` when `addpylib\s+(.+)\s+(.+)` matches (`re.match`, greedy), `Some(None)`
/// when it does not (one argument: "unparsed line"), `None` for any other line.
pub(crate) fn add_pylib_line(text: &str) -> Option<Option<(String, String)>> {
    let rest = text.strip_prefix("addpylib")?;
    if !rest.starts_with(crate::parse::lexer::py_space) {
        return None;
    }
    let chars: Vec<char> = rest.chars().collect();
    Some(match_groups(&chars, 0, 2).and_then(|g| match g.as_slice() {
        [(a, b), (c, d)] => Some((
            chars[*a..*b].iter().collect(),
            chars[*c..*d].iter().collect(),
        )),
        _ => None,
    }))
}

/// [`AddFragments`] for a logical line starting with `addfragments` and whitespace.
pub(crate) fn add_fragments_line(text: &str) -> Option<AddFragments> {
    let rest = text.strip_prefix("addfragments")?;
    rest.starts_with(crate::parse::lexer::py_space)
        .then(|| AddFragments {
            rest: rest.to_string(),
        })
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

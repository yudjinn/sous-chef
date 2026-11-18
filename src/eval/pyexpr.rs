//! A strict subset of python expressions, so that the inline python of the pure helpers
//! (`${@bb.utils.contains('DISTRO_FEATURES', 'x', 'a', 'b', d)}`) is evaluated statically.
//!
//! BitBake runs `${@code}` with `eval(compile(code.strip(), ..., "eval"))` and substitutes
//! `str(result)` (`data_smart.py` `VariableParse.python_sub`). This module parses `code` and
//! evaluates it **only** when it is made of the constructs below, each with python's exact
//! semantics; anything else, anywhere in the expression, makes [`parse`] fail and the caller
//! leaves the expression unevaluated (and flagged), exactly as before:
//!
//! - literals: `'...'` / `"..."` strings with the escapes `\\ \' \" \n \t` (no prefixes,
//!   no triple quotes, no implicit concatenation), decimal integers, `True`, `False`, `None`,
//!   lists `[a, b]`;
//! - the name `d` (the datastore, only as an argument or for `d.getVar` / `d.getVarFlag` /
//!   `d.expand`);
//! - calls to the allowlisted helpers ([`Func`]), with positional and keyword arguments as
//!   their python signatures accept them, and to `len`;
//! - `a + b` (strings, lists), `a if c else b`, `and` / `or` / `not`, one `==` / `!=` / `in`
//!   / `not in` per comparison, `-` before an integer literal;
//! - a subscript `x[i]` of a list by an integer or a boolean (`['', 'x'][cond]`);
//! - the string methods `split()` / `split(sep)`, `strip()` / `strip(chars)`,
//!   `replace(old, new)`, `startswith(s)`, `endswith(s)`, `lower()`, `upper()` (ASCII text
//!   only), `sep.join(list)`.
//!
//! A python error (an `AttributeError` on `None`, an `IndexError`, a `ValueError` in
//! `to_boolean`) is never reproduced: evaluation stops with [`Stop::Unsupported`], since BitBake
//! would fail to expand the value. A helper whose result depends on the iteration order of a
//! python `set` of several strings (`oe.utils.both_contain` with several words,
//! `oe.utils.set_intersect`) is not evaluated either. The semantics of every helper, with its
//! source, are in `docs/bitbake-semantics.md` ("Inline python evaluated statically").

use std::cmp::Ordering;

/// Maximum nesting of the parser (brackets, calls, operators).
const MAX_NESTING: usize = 64;

/// Why an expression was not evaluated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// Outside the subset, or python would raise.
    Unsupported(String),
    /// A value it reads is not certain (the reason names the variable).
    Uncertain(String),
}

/// What the expression reads from the datastore. Every method returns a *certain* value or
/// stops.
pub trait Env {
    /// `d.getVar(name)`: the finalised, expanded value, `None` when unset.
    fn get_var(&mut self, name: &str) -> Result<Option<String>, Stop>;
    /// `d.getVarFlag(name, flag)`: the expanded flag, `None` when unset.
    fn get_var_flag(&mut self, name: &str, flag: &str) -> Result<Option<String>, Stop>;
    /// `d.expand(text)`.
    fn expand(&mut self, text: &str) -> Result<String, Stop>;
}

/// A python value of the subset.
#[derive(Debug, Clone, PartialEq)]
pub enum PyVal {
    None,
    Bool(bool),
    Int(i128),
    Str(String),
    List(Vec<PyVal>),
    /// The datastore `d`.
    D,
}

impl PyVal {
    /// Python truth value.
    fn truthy(&self) -> Result<bool, Stop> {
        Ok(match self {
            PyVal::None => false,
            PyVal::Bool(b) => *b,
            PyVal::Int(i) => *i != 0,
            PyVal::Str(s) => !s.is_empty(),
            PyVal::List(l) => !l.is_empty(),
            PyVal::D => return Err(unsupported("the truth value of d")),
        })
    }

    fn type_name(&self) -> &'static str {
        match self {
            PyVal::None => "None",
            PyVal::Bool(_) => "bool",
            PyVal::Int(_) => "int",
            PyVal::Str(_) => "str",
            PyVal::List(_) => "list",
            PyVal::D => "d",
        }
    }
}

/// `str(value)`, as `python_sub` substitutes it. A list (python's `repr`) and `d` are not
/// reproduced.
pub fn to_text(v: &PyVal) -> Result<String, Stop> {
    match v {
        PyVal::None => Ok("None".into()),
        PyVal::Bool(true) => Ok("True".into()),
        PyVal::Bool(false) => Ok("False".into()),
        PyVal::Int(i) => Ok(i.to_string()),
        PyVal::Str(s) => Ok(s.clone()),
        PyVal::List(_) => Err(unsupported("str() of a list")),
        PyVal::D => Err(unsupported("str() of d")),
    }
}

fn unsupported(why: impl Into<String>) -> Stop {
    Stop::Unsupported(why.into())
}

/// A short form of `code` for a note: whole when short, else the called name with its first
/// argument (`bb.utils.contains('DISTRO_FEATURES', ...)`), else cut.
pub fn short(code: &str) -> String {
    const MAX: usize = 60;
    let code = code.trim();
    if code.chars().count() <= MAX {
        return code.to_string();
    }
    let name_len = code
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'.')
        .count();
    if name_len > 0 && code.as_bytes().get(name_len) == Some(&b'(') {
        let rest = &code[name_len + 1..];
        let first: String = rest
            .chars()
            .take_while(|c| *c != ',' && *c != ')')
            .collect();
        let s = format!("{}({}, ...)", &code[..name_len], first.trim());
        if s.chars().count() <= MAX + 10 {
            return s;
        }
        return format!("{}(...)", &code[..name_len]);
    }
    let cut: String = code.chars().take(MAX - 3).collect();
    format!("{cut}...")
}

// ---------------------------------------------------------------------------------------
// Tokens

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Name(String),
    Str(String),
    Int(i128),
    Op(&'static str),
}

/// Python's whitespace for `str.split()` / `str.strip()` (`Py_UNICODE_ISSPACE`): Rust's
/// `char::is_whitespace` plus U+001C..U+001F.
pub fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// True if `code` holds a string literal python would report as unterminated (a quote with
/// no closing quote on the line, a triple quote with no closing triple quote). BitBake
/// swallows that `SyntaxError` for the **whole** substitution pass (`expandWithRefs`), so
/// then no expression of the text is replaced.
pub fn has_unterminated_string(code: &str) -> bool {
    let c: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '#' => return false,
            q @ ('\'' | '"') => {
                if c.get(i + 1) == Some(&q) && c.get(i + 2) == Some(&q) {
                    let mut j = i + 3;
                    loop {
                        if j + 3 > c.len() {
                            return true;
                        }
                        if c[j] == '\\' {
                            j += 2;
                            continue;
                        }
                        if c[j] == q && c[j + 1] == q && c[j + 2] == q {
                            i = j + 3;
                            break;
                        }
                        j += 1;
                    }
                    continue;
                }
                let mut j = i + 1;
                loop {
                    if j >= c.len() {
                        return true;
                    }
                    if c[j] == '\\' {
                        j += 2;
                        continue;
                    }
                    if c[j] == q {
                        break;
                    }
                    j += 1;
                }
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    false
}

fn tokenize(code: &str) -> Result<Vec<Tok>, Stop> {
    let c: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch == ' ' || ch == '\t' {
            i += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            if c.get(i + 1) == Some(&ch) && c.get(i + 2) == Some(&ch) {
                return Err(unsupported("a triple-quoted string"));
            }
            let mut s = String::new();
            let mut j = i + 1;
            loop {
                let Some(&x) = c.get(j) else {
                    return Err(unsupported("an unterminated string"));
                };
                if x == ch {
                    break;
                }
                if x == '\\' {
                    let e = c.get(j + 1).copied();
                    match e {
                        Some('\\') => s.push('\\'),
                        Some('\'') => s.push('\''),
                        Some('"') => s.push('"'),
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        _ => return Err(unsupported("a string escape outside the subset")),
                    }
                    j += 2;
                    continue;
                }
                s.push(x);
                j += 1;
            }
            out.push(Tok::Str(s));
            i = j + 1;
            if matches!(c.get(i), Some('\'' | '"')) {
                return Err(unsupported("implicit string concatenation"));
            }
            continue;
        }
        if ch.is_ascii_digit() {
            let mut j = i;
            while j < c.len() && c[j].is_ascii_digit() {
                j += 1;
            }
            if j < c.len() && (c[j].is_alphanumeric() || c[j] == '_' || c[j] == '.') {
                return Err(unsupported("a number outside the subset"));
            }
            let digits: String = c[i..j].iter().collect();
            if digits.len() > 1 && digits.starts_with('0') {
                return Err(unsupported("an integer with leading zeros"));
            }
            let n: i128 = digits
                .parse()
                .map_err(|_| unsupported("an integer too large"))?;
            out.push(Tok::Int(n));
            i = j;
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let mut j = i;
            while j < c.len() && (c[j].is_ascii_alphanumeric() || c[j] == '_') {
                j += 1;
            }
            if j < c.len() && !c[j].is_ascii() {
                return Err(unsupported("a non-ASCII name"));
            }
            if matches!(c.get(j), Some('\'' | '"')) {
                return Err(unsupported("a string prefix"));
            }
            out.push(Tok::Name(c[i..j].iter().collect()));
            i = j;
            continue;
        }
        let two: String = c[i..(i + 2).min(c.len())].iter().collect();
        let op: &'static str = match two.as_str() {
            "==" => "==",
            "!=" => "!=",
            _ => match ch {
                '(' => "(",
                ')' => ")",
                '[' => "[",
                ']' => "]",
                ',' => ",",
                '.' => ".",
                '+' => "+",
                '-' => "-",
                '=' => "=",
                _ => return Err(unsupported(format!("`{ch}`"))),
            },
        };
        // `==` / `!=` are two characters; `<=`, `>=`, `+=` and the like are rejected by the
        // single-character table (`<`, `>`) or by the parser (`+` then `=`).
        i += op.len();
        out.push(Tok::Op(op));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Syntax

/// An allowlisted function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Func {
    BbContains,
    BbContainsAny,
    BbFilter,
    BbToBoolean,
    BbVercmpString,
    OeConditional,
    OeIfelse,
    OeLessOrEqual,
    OeVersionLessOrEqual,
    OeBothContain,
    OePruneSuffix,
    OeAllDistroFeatures,
    OeAnyDistroFeatures,
    OeVartrue,
    OeSetDifference,
    OeFilterDefaultFeatures,
    DGetVar,
    DGetVarFlag,
    DExpand,
    Len,
}

impl Func {
    fn bb_utils(name: &str) -> Option<Func> {
        Some(match name {
            "contains" => Func::BbContains,
            "contains_any" => Func::BbContainsAny,
            "filter" => Func::BbFilter,
            "to_boolean" => Func::BbToBoolean,
            "vercmp_string" => Func::BbVercmpString,
            _ => return None,
        })
    }

    fn oe_utils(name: &str) -> Option<Func> {
        Some(match name {
            "conditional" => Func::OeConditional,
            "ifelse" => Func::OeIfelse,
            "less_or_equal" => Func::OeLessOrEqual,
            "version_less_or_equal" => Func::OeVersionLessOrEqual,
            "both_contain" => Func::OeBothContain,
            "prune_suffix" => Func::OePruneSuffix,
            "all_distro_features" => Func::OeAllDistroFeatures,
            "any_distro_features" => Func::OeAnyDistroFeatures,
            "vartrue" => Func::OeVartrue,
            "set_difference" => Func::OeSetDifference,
            "filter_default_features" => Func::OeFilterDefaultFeatures,
            _ => return None,
        })
    }

    fn d_method(name: &str) -> Option<Func> {
        Some(match name {
            "getVar" => Func::DGetVar,
            "getVarFlag" => Func::DGetVarFlag,
            "expand" => Func::DExpand,
            _ => return None,
        })
    }

    /// The python parameters: name and default (`None` = required).
    fn params(self) -> &'static [(&'static str, Option<Dflt>)] {
        use Dflt as D;
        match self {
            Func::BbContains | Func::BbContainsAny => &[
                ("variable", None),
                ("checkvalues", None),
                ("truevalue", None),
                ("falsevalue", None),
                ("d", None),
            ],
            Func::BbFilter => &[("variable", None), ("checkvalues", None), ("d", None)],
            Func::BbToBoolean => &[("string", None), ("default", Some(D::None))],
            Func::BbVercmpString => &[("a", None), ("b", None)],
            Func::OeConditional | Func::OeLessOrEqual | Func::OeVersionLessOrEqual => &[
                ("variable", None),
                ("checkvalue", None),
                ("truevalue", None),
                ("falsevalue", None),
                ("d", None),
            ],
            Func::OeIfelse => &[
                ("condition", None),
                ("iftrue", Some(D::True)),
                ("iffalse", Some(D::False)),
            ],
            Func::OeBothContain => &[
                ("variable1", None),
                ("variable2", None),
                ("checkvalue", None),
                ("d", None),
            ],
            Func::OePruneSuffix => &[("var", None), ("suffixes", None), ("d", None)],
            Func::OeAllDistroFeatures | Func::OeAnyDistroFeatures => &[
                ("d", None),
                ("features", None),
                ("truevalue", Some(D::One)),
                ("falsevalue", Some(D::Empty)),
            ],
            Func::OeVartrue => &[
                ("var", None),
                ("iftrue", None),
                ("iffalse", None),
                ("d", None),
            ],
            Func::OeSetDifference => &[("variable1", None), ("variable2", None), ("d", None)],
            Func::OeFilterDefaultFeatures => &[("varname", None), ("d", None)],
            // `getVar(self, var, expand=True, noweakdefault=False, parsing=False)`: only the
            // first two are accepted.
            Func::DGetVar => &[("var", None), ("expand", Some(D::True))],
            // `getVarFlag(self, var, flag, expand=True, ...)`.
            Func::DGetVarFlag => &[("var", None), ("flag", None), ("expand", Some(D::True))],
            // `expand(self, s, varname=None)`: `varname` is not accepted.
            Func::DExpand => &[("s", None)],
            Func::Len => &[("obj", None)],
        }
    }

    /// The name as written, for notes.
    pub fn name(self) -> &'static str {
        match self {
            Func::BbContains => "bb.utils.contains",
            Func::BbContainsAny => "bb.utils.contains_any",
            Func::BbFilter => "bb.utils.filter",
            Func::BbToBoolean => "bb.utils.to_boolean",
            Func::BbVercmpString => "bb.utils.vercmp_string",
            Func::OeConditional => "oe.utils.conditional",
            Func::OeIfelse => "oe.utils.ifelse",
            Func::OeLessOrEqual => "oe.utils.less_or_equal",
            Func::OeVersionLessOrEqual => "oe.utils.version_less_or_equal",
            Func::OeBothContain => "oe.utils.both_contain",
            Func::OePruneSuffix => "oe.utils.prune_suffix",
            Func::OeAllDistroFeatures => "oe.utils.all_distro_features",
            Func::OeAnyDistroFeatures => "oe.utils.any_distro_features",
            Func::OeVartrue => "oe.utils.vartrue",
            Func::OeSetDifference => "oe.utils.set_difference",
            Func::OeFilterDefaultFeatures => "oe.utils.filter_default_features",
            Func::DGetVar => "d.getVar",
            Func::DGetVarFlag => "d.getVarFlag",
            Func::DExpand => "d.expand",
            Func::Len => "len",
        }
    }
}

/// A parameter default.
#[derive(Debug, Clone, Copy)]
enum Dflt {
    None,
    True,
    False,
    One,
    Empty,
}

impl Dflt {
    fn value(self) -> PyVal {
        match self {
            Dflt::None => PyVal::None,
            Dflt::True => PyVal::Bool(true),
            Dflt::False => PyVal::Bool(false),
            Dflt::One => PyVal::Str("1".into()),
            Dflt::Empty => PyVal::Str(String::new()),
        }
    }
}

/// A string method of the subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    Split,
    Strip,
    Replace,
    Startswith,
    Endswith,
    Lower,
    Upper,
    Join,
}

impl Method {
    fn from_name(s: &str) -> Option<Method> {
        Some(match s {
            "split" => Method::Split,
            "strip" => Method::Strip,
            "replace" => Method::Replace,
            "startswith" => Method::Startswith,
            "endswith" => Method::Endswith,
            "lower" => Method::Lower,
            "upper" => Method::Upper,
            "join" => Method::Join,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmpOp {
    Eq,
    Ne,
    In,
    NotIn,
}

/// A parsed expression of the subset.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr(Node);

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Lit(PyVal),
    List(Vec<Node>),
    Call {
        func: Func,
        args: Vec<Node>,
        kwargs: Vec<(String, Node)>,
    },
    Method {
        recv: Box<Node>,
        method: Method,
        args: Vec<Node>,
    },
    Index(Box<Node>, Box<Node>),
    Add(Box<Node>, Box<Node>),
    Not(Box<Node>),
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    Cmp(CmpOp, Box<Node>, Box<Node>),
    IfElse {
        body: Box<Node>,
        cond: Box<Node>,
        orelse: Box<Node>,
    },
}

impl Expr {
    /// The allowlisted functions it calls, in order of first appearance.
    pub fn funcs(&self) -> Vec<Func> {
        let mut out = Vec::new();
        self.0.funcs(&mut out);
        out
    }
}

impl Node {
    fn funcs(&self, out: &mut Vec<Func>) {
        match self {
            Node::Lit(_) => {}
            Node::List(v) => v.iter().for_each(|n| n.funcs(out)),
            Node::Call { func, args, kwargs } => {
                if !out.contains(func) {
                    out.push(*func);
                }
                args.iter().for_each(|n| n.funcs(out));
                kwargs.iter().for_each(|(_, n)| n.funcs(out));
            }
            Node::Method { recv, args, .. } => {
                recv.funcs(out);
                args.iter().for_each(|n| n.funcs(out));
            }
            Node::Index(a, b) | Node::Add(a, b) | Node::And(a, b) | Node::Or(a, b) => {
                a.funcs(out);
                b.funcs(out);
            }
            Node::Cmp(_, a, b) => {
                a.funcs(out);
                b.funcs(out);
            }
            Node::Not(a) => a.funcs(out),
            Node::IfElse { body, cond, orelse } => {
                body.funcs(out);
                cond.funcs(out);
                orelse.funcs(out);
            }
        }
    }
}

/// Parses `code` (the text between `${@` and `}`; python's surrounding whitespace is
/// stripped first, as `python_sub` does). `Err` when anything is outside the subset.
pub fn parse(code: &str) -> Result<Expr, Stop> {
    let toks = tokenize(code.trim_matches(py_space))?;
    if toks.is_empty() {
        return Err(unsupported("an empty expression"));
    }
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let n = p.expr()?;
    if p.pos != p.toks.len() {
        return Err(unsupported("text after the expression"));
    }
    Ok(Expr(n))
}

/// Positional and keyword arguments of a call.
type Args = (Vec<Node>, Vec<(String, Node)>);

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn peek_at(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.pos + k)
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if *o == op)
    }

    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Tok::Name(n)) if n == kw)
    }

    fn eat_op(&mut self, op: &str) -> Result<(), Stop> {
        if self.is_op(op) {
            self.pos += 1;
            Ok(())
        } else {
            Err(unsupported(format!("expected `{op}`")))
        }
    }

    fn name(&mut self) -> Result<String, Stop> {
        match self.peek() {
            Some(Tok::Name(n)) => {
                let n = n.clone();
                self.pos += 1;
                Ok(n)
            }
            _ => Err(unsupported("expected a name")),
        }
    }

    fn nest(&mut self) -> Result<(), Stop> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(unsupported("nesting too deep"));
        }
        Ok(())
    }

    /// `or_test ['if' or_test 'else' test]`
    fn expr(&mut self) -> Result<Node, Stop> {
        self.nest()?;
        let body = self.or_expr()?;
        let n = if self.is_kw("if") {
            self.pos += 1;
            let cond = self.or_expr()?;
            if !self.is_kw("else") {
                return Err(unsupported("`if` without `else`"));
            }
            self.pos += 1;
            let orelse = self.expr()?;
            Node::IfElse {
                body: Box::new(body),
                cond: Box::new(cond),
                orelse: Box::new(orelse),
            }
        } else {
            body
        };
        self.depth -= 1;
        Ok(n)
    }

    fn or_expr(&mut self) -> Result<Node, Stop> {
        let mut l = self.and_expr()?;
        while self.is_kw("or") {
            self.pos += 1;
            let r = self.and_expr()?;
            l = Node::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn and_expr(&mut self) -> Result<Node, Stop> {
        let mut l = self.not_expr()?;
        while self.is_kw("and") {
            self.pos += 1;
            let r = self.not_expr()?;
            l = Node::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn not_expr(&mut self) -> Result<Node, Stop> {
        if self.is_kw("not") {
            self.pos += 1;
            self.nest()?;
            let n = self.not_expr()?;
            self.depth -= 1;
            return Ok(Node::Not(Box::new(n)));
        }
        self.comparison()
    }

    fn cmp_op(&mut self) -> Option<CmpOp> {
        let op = match self.peek() {
            Some(Tok::Op("==")) => CmpOp::Eq,
            Some(Tok::Op("!=")) => CmpOp::Ne,
            Some(Tok::Name(n)) if n == "in" => CmpOp::In,
            Some(Tok::Name(n))
                if n == "not" && matches!(self.peek_at(1), Some(Tok::Name(m)) if m == "in") =>
            {
                self.pos += 1;
                CmpOp::NotIn
            }
            _ => return None,
        };
        self.pos += 1;
        Some(op)
    }

    fn comparison(&mut self) -> Result<Node, Stop> {
        let l = self.sum()?;
        let Some(op) = self.cmp_op() else {
            if self.is_kw("is") {
                return Err(unsupported("`is`"));
            }
            return Ok(l);
        };
        let r = self.sum()?;
        if self.cmp_op().is_some() || self.is_kw("is") {
            return Err(unsupported("a chained comparison"));
        }
        Ok(Node::Cmp(op, Box::new(l), Box::new(r)))
    }

    fn sum(&mut self) -> Result<Node, Stop> {
        let mut l = self.unary()?;
        while self.is_op("+") {
            self.pos += 1;
            let r = self.unary()?;
            l = Node::Add(Box::new(l), Box::new(r));
        }
        if self.is_op("-") {
            return Err(unsupported("subtraction"));
        }
        Ok(l)
    }

    fn unary(&mut self) -> Result<Node, Stop> {
        if self.is_op("-") {
            // Only a negative integer literal (a list index such as `[-1]`).
            if let Some(Tok::Int(n)) = self.peek_at(1) {
                let n = *n;
                // A trailer after the literal (`-1 .x`) would bind tighter than the minus.
                if !matches!(self.peek_at(2), Some(Tok::Op("." | "(" | "["))) {
                    self.pos += 2;
                    return Ok(Node::Lit(PyVal::Int(-n)));
                }
            }
            return Err(unsupported("unary minus"));
        }
        if self.is_op("+") {
            return Err(unsupported("unary plus"));
        }
        self.primary()
    }

    fn args(&mut self) -> Result<Args, Stop> {
        // After `(`.
        let mut args = Vec::new();
        let mut kwargs: Vec<(String, Node)> = Vec::new();
        self.nest()?;
        loop {
            if self.is_op(")") {
                self.pos += 1;
                break;
            }
            let kw = match (self.peek(), self.peek_at(1)) {
                (Some(Tok::Name(n)), Some(Tok::Op("="))) if !KEYWORDS.contains(&n.as_str()) => {
                    Some(n.clone())
                }
                _ => None,
            };
            if let Some(k) = kw {
                self.pos += 2;
                let v = self.expr()?;
                if kwargs.iter().any(|(n, _)| *n == k) {
                    return Err(unsupported("a repeated keyword argument"));
                }
                kwargs.push((k, v));
            } else {
                if !kwargs.is_empty() {
                    return Err(unsupported(
                        "a positional argument after a keyword argument",
                    ));
                }
                args.push(self.expr()?);
            }
            if self.is_op(",") {
                self.pos += 1;
            } else if !self.is_op(")") {
                return Err(unsupported("expected `,` or `)`"));
            }
        }
        self.depth -= 1;
        Ok((args, kwargs))
    }

    fn call(&mut self, func: Func) -> Result<Node, Stop> {
        self.eat_op("(")?;
        let (args, kwargs) = self.args()?;
        Ok(Node::Call { func, args, kwargs })
    }

    fn atom(&mut self) -> Result<Node, Stop> {
        let Some(t) = self.peek().cloned() else {
            return Err(unsupported("an incomplete expression"));
        };
        match t {
            Tok::Str(s) => {
                self.pos += 1;
                Ok(Node::Lit(PyVal::Str(s)))
            }
            Tok::Int(n) => {
                self.pos += 1;
                Ok(Node::Lit(PyVal::Int(n)))
            }
            Tok::Op("(") => {
                self.pos += 1;
                if self.is_op(")") {
                    return Err(unsupported("a tuple"));
                }
                let n = self.expr()?;
                if self.is_op(",") {
                    return Err(unsupported("a tuple"));
                }
                self.eat_op(")")?;
                Ok(n)
            }
            Tok::Op("[") => {
                self.pos += 1;
                self.nest()?;
                let mut items = Vec::new();
                loop {
                    if self.is_op("]") {
                        self.pos += 1;
                        break;
                    }
                    items.push(self.expr()?);
                    if self.is_kw("for") {
                        return Err(unsupported("a comprehension"));
                    }
                    if self.is_op(",") {
                        self.pos += 1;
                    } else if !self.is_op("]") {
                        return Err(unsupported("expected `,` or `]`"));
                    }
                }
                self.depth -= 1;
                Ok(Node::List(items))
            }
            Tok::Name(n) => {
                self.pos += 1;
                match n.as_str() {
                    "True" => Ok(Node::Lit(PyVal::Bool(true))),
                    "False" => Ok(Node::Lit(PyVal::Bool(false))),
                    "None" => Ok(Node::Lit(PyVal::None)),
                    "len" if self.is_op("(") => self.call(Func::Len),
                    "d" => {
                        if self.is_op(".") {
                            self.pos += 1;
                            let m = self.name()?;
                            let f = Func::d_method(&m).ok_or_else(|| {
                                unsupported(format!("d.{m} is not on the allowlist"))
                            })?;
                            self.call(f)
                        } else {
                            Ok(Node::Lit(PyVal::D))
                        }
                    }
                    "bb" | "oe" => {
                        self.eat_op(".")?;
                        let module = self.name()?;
                        if module != "utils" {
                            return Err(unsupported(format!(
                                "{n}.{module} is not on the allowlist"
                            )));
                        }
                        self.eat_op(".")?;
                        let f = self.name()?;
                        let func = if n == "bb" {
                            Func::bb_utils(&f)
                        } else {
                            Func::oe_utils(&f)
                        }
                        .ok_or_else(|| {
                            unsupported(format!("{n}.utils.{f} is not on the allowlist"))
                        })?;
                        self.call(func)
                    }
                    _ => Err(unsupported(format!("the name `{n}`"))),
                }
            }
            Tok::Op(o) => Err(unsupported(format!("`{o}`"))),
        }
    }

    fn primary(&mut self) -> Result<Node, Stop> {
        let mut n = self.atom()?;
        loop {
            if self.is_op(".") {
                self.pos += 1;
                let m = self.name()?;
                let method = Method::from_name(&m)
                    .ok_or_else(|| unsupported(format!("the method `.{m}`")))?;
                if !self.is_op("(") {
                    return Err(unsupported("an attribute that is not called"));
                }
                self.pos += 1;
                let (args, kwargs) = self.args()?;
                if !kwargs.is_empty() {
                    return Err(unsupported("keyword arguments to a string method"));
                }
                n = Node::Method {
                    recv: Box::new(n),
                    method,
                    args,
                };
            } else if self.is_op("[") {
                self.pos += 1;
                let i = self.expr()?;
                if !self.is_op("]") {
                    return Err(unsupported("a slice or a tuple index"));
                }
                self.pos += 1;
                n = Node::Index(Box::new(n), Box::new(i));
            } else if self.is_op("(") {
                return Err(unsupported("a call of something not on the allowlist"));
            } else {
                return Ok(n);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Evaluation

/// Evaluates `e` against `env`.
pub fn eval(e: &Expr, env: &mut dyn Env) -> Result<PyVal, Stop> {
    eval_node(&e.0, env)
}

fn eval_node(n: &Node, env: &mut dyn Env) -> Result<PyVal, Stop> {
    match n {
        Node::Lit(v) => Ok(v.clone()),
        Node::List(items) => Ok(PyVal::List(
            items
                .iter()
                .map(|i| eval_node(i, env))
                .collect::<Result<_, _>>()?,
        )),
        Node::Add(a, b) => {
            let (a, b) = (eval_node(a, env)?, eval_node(b, env)?);
            match (a, b) {
                (PyVal::Str(mut x), PyVal::Str(y)) => {
                    x.push_str(&y);
                    Ok(PyVal::Str(x))
                }
                (PyVal::List(mut x), PyVal::List(y)) => {
                    x.extend(y);
                    Ok(PyVal::List(x))
                }
                (a, b) => Err(unsupported(format!(
                    "{} + {} (a TypeError or arithmetic)",
                    a.type_name(),
                    b.type_name()
                ))),
            }
        }
        Node::Not(a) => Ok(PyVal::Bool(!eval_node(a, env)?.truthy()?)),
        Node::And(a, b) => {
            let l = eval_node(a, env)?;
            if l.truthy()? {
                eval_node(b, env)
            } else {
                Ok(l)
            }
        }
        Node::Or(a, b) => {
            let l = eval_node(a, env)?;
            if l.truthy()? {
                Ok(l)
            } else {
                eval_node(b, env)
            }
        }
        Node::Cmp(op, a, b) => {
            let (a, b) = (eval_node(a, env)?, eval_node(b, env)?);
            let r = match op {
                CmpOp::Eq => py_eq(&a, &b)?,
                CmpOp::Ne => !py_eq(&a, &b)?,
                CmpOp::In => py_in(&a, &b)?,
                CmpOp::NotIn => !py_in(&a, &b)?,
            };
            Ok(PyVal::Bool(r))
        }
        Node::IfElse { body, cond, orelse } => {
            if eval_node(cond, env)?.truthy()? {
                eval_node(body, env)
            } else {
                eval_node(orelse, env)
            }
        }
        Node::Index(base, idx) => {
            let (base, idx) = (eval_node(base, env)?, eval_node(idx, env)?);
            let PyVal::List(items) = base else {
                return Err(unsupported(format!("a subscript of {}", base.type_name())));
            };
            let i = match idx {
                PyVal::Int(i) => i,
                PyVal::Bool(b) => i128::from(b),
                other => {
                    return Err(unsupported(format!("a {} index", other.type_name())));
                }
            };
            let len = items.len() as i128;
            let k = if i < 0 { i + len } else { i };
            if k < 0 || k >= len {
                return Err(unsupported("an IndexError"));
            }
            Ok(items[k as usize].clone())
        }
        Node::Method { recv, method, args } => {
            let recv = eval_node(recv, env)?;
            let args: Vec<PyVal> = args
                .iter()
                .map(|a| eval_node(a, env))
                .collect::<Result<_, _>>()?;
            string_method(recv, *method, args)
        }
        Node::Call { func, args, kwargs } => {
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(eval_node(a, env)?);
            }
            let mut kvals = Vec::with_capacity(kwargs.len());
            for (k, a) in kwargs {
                kvals.push((k.clone(), eval_node(a, env)?));
            }
            let bound = bind(*func, vals, kvals)?;
            call(*func, bound, env)
        }
    }
}

/// Python's binding of positional and keyword arguments to `func`'s parameters.
fn bind(func: Func, args: Vec<PyVal>, kwargs: Vec<(String, PyVal)>) -> Result<Vec<PyVal>, Stop> {
    let params = func.params();
    if args.len() > params.len() {
        return Err(unsupported(format!(
            "too many arguments to {}",
            func.name()
        )));
    }
    let mut out: Vec<Option<PyVal>> = vec![None; params.len()];
    for (i, a) in args.into_iter().enumerate() {
        out[i] = Some(a);
    }
    for (k, v) in kwargs {
        let Some(i) = params.iter().position(|(n, _)| *n == k) else {
            return Err(unsupported(format!(
                "the keyword argument `{k}` of {}",
                func.name()
            )));
        };
        if out[i].is_some() {
            return Err(unsupported("an argument given twice"));
        }
        out[i] = Some(v);
    }
    out.into_iter()
        .zip(params)
        .map(|(v, (n, dflt))| match (v, dflt) {
            (Some(v), _) => {
                if *n == "d" && v != PyVal::D {
                    return Err(unsupported("a datastore other than d"));
                }
                if *n != "d" && v == PyVal::D {
                    return Err(unsupported("d passed where a value is expected"));
                }
                Ok(v)
            }
            (None, Some(dv)) => Ok(dv.value()),
            (None, None) => Err(unsupported(format!(
                "a missing argument `{n}` to {}",
                func.name()
            ))),
        })
        .collect()
}

fn want_str<'a>(v: &'a PyVal, what: &str) -> Result<&'a str, Stop> {
    match v {
        PyVal::Str(s) => Ok(s),
        other => Err(unsupported(format!("{what} is {}", other.type_name()))),
    }
}

/// `str.split()`: runs of python whitespace.
pub fn py_split_ws(s: &str) -> Vec<String> {
    s.split(py_space)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// `set(checkvalues)` as `bb.utils.contains` builds it: a string is split on whitespace,
/// anything else iterated (here: a list). The elements, without duplicates.
fn check_set(v: &PyVal) -> Result<Vec<PyVal>, Stop> {
    let mut out: Vec<PyVal> = Vec::new();
    let items: Vec<PyVal> = match v {
        PyVal::Str(s) => py_split_ws(s).into_iter().map(PyVal::Str).collect(),
        PyVal::List(l) => l.clone(),
        other => {
            return Err(unsupported(format!("set() of {}", other.type_name())));
        }
    };
    for i in items {
        if matches!(i, PyVal::List(_) | PyVal::D) {
            return Err(unsupported("an unhashable set element"));
        }
        if !out.iter().any(|o| py_eq(o, &i).unwrap_or(false)) {
            out.push(i);
        }
    }
    Ok(out)
}

/// Python `==`.
fn py_eq(a: &PyVal, b: &PyVal) -> Result<bool, Stop> {
    Ok(match (a, b) {
        (PyVal::D, _) | (_, PyVal::D) => return Err(unsupported("a comparison with d")),
        (PyVal::None, PyVal::None) => true,
        (PyVal::Str(x), PyVal::Str(y)) => x == y,
        (PyVal::Bool(x), PyVal::Bool(y)) => x == y,
        (PyVal::Int(x), PyVal::Int(y)) => x == y,
        (PyVal::Bool(x), PyVal::Int(y)) | (PyVal::Int(y), PyVal::Bool(x)) => i128::from(*x) == *y,
        (PyVal::List(x), PyVal::List(y)) => {
            if x.len() != y.len() {
                return Ok(false);
            }
            for (p, q) in x.iter().zip(y) {
                if !py_eq(p, q)? {
                    return Ok(false);
                }
            }
            true
        }
        _ => false,
    })
}

/// Python `a in b`.
fn py_in(a: &PyVal, b: &PyVal) -> Result<bool, Stop> {
    match b {
        PyVal::Str(hay) => match a {
            PyVal::Str(needle) => Ok(hay.contains(needle.as_str())),
            other => Err(unsupported(format!("{} in str", other.type_name()))),
        },
        PyVal::List(items) => {
            for i in items {
                if py_eq(a, i)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        other => Err(unsupported(format!("`in` {}", other.type_name()))),
    }
}

fn string_method(recv: PyVal, m: Method, args: Vec<PyVal>) -> Result<PyVal, Stop> {
    let PyVal::Str(s) = recv else {
        return Err(unsupported(format!(
            "a string method on {} (an AttributeError)",
            recv.type_name()
        )));
    };
    let nargs = |n: std::ops::RangeInclusive<usize>| -> Result<(), Stop> {
        if n.contains(&args.len()) {
            Ok(())
        } else {
            Err(unsupported("a string method with other arguments"))
        }
    };
    match m {
        Method::Split => {
            nargs(0..=1)?;
            match args.first() {
                None | Some(PyVal::None) => Ok(PyVal::List(
                    py_split_ws(&s).into_iter().map(PyVal::Str).collect(),
                )),
                Some(PyVal::Str(sep)) if !sep.is_empty() => Ok(PyVal::List(
                    s.split(sep.as_str())
                        .map(|p| PyVal::Str(p.to_string()))
                        .collect(),
                )),
                Some(_) => Err(unsupported("split() with that separator")),
            }
        }
        Method::Strip => {
            nargs(0..=1)?;
            match args.first() {
                None | Some(PyVal::None) => Ok(PyVal::Str(s.trim_matches(py_space).to_string())),
                Some(PyVal::Str(chars)) => {
                    let set: Vec<char> = chars.chars().collect();
                    Ok(PyVal::Str(s.trim_matches(|c| set.contains(&c)).to_string()))
                }
                Some(_) => Err(unsupported("strip() with that argument")),
            }
        }
        Method::Replace => {
            nargs(2..=2)?;
            let old = want_str(&args[0], "the first argument of replace()")?;
            let new = want_str(&args[1], "the second argument of replace()")?;
            Ok(PyVal::Str(s.replace(old, new)))
        }
        Method::Startswith | Method::Endswith => {
            nargs(1..=1)?;
            let x = want_str(&args[0], "the argument of startswith() / endswith()")?;
            Ok(PyVal::Bool(if m == Method::Startswith {
                s.starts_with(x)
            } else {
                s.ends_with(x)
            }))
        }
        Method::Lower | Method::Upper => {
            nargs(0..=0)?;
            if !s.is_ascii() {
                return Err(unsupported("lower() / upper() of non-ASCII text"));
            }
            Ok(PyVal::Str(if m == Method::Lower {
                s.to_ascii_lowercase()
            } else {
                s.to_ascii_uppercase()
            }))
        }
        Method::Join => {
            nargs(1..=1)?;
            let parts: Vec<String> = match &args[0] {
                PyVal::List(items) => items
                    .iter()
                    .map(|i| want_str(i, "a joined item").map(str::to_string))
                    .collect::<Result<_, _>>()?,
                PyVal::Str(t) => t.chars().map(String::from).collect(),
                other => {
                    return Err(unsupported(format!("join() of {}", other.type_name())));
                }
            };
            Ok(PyVal::Str(parts.join(&s)))
        }
    }
}

/// `d.getVar(name)` with a string name.
fn get_var(env: &mut dyn Env, name: &PyVal) -> Result<PyVal, Stop> {
    let n = want_str(name, "the variable name")?;
    Ok(match env.get_var(n)? {
        Some(v) => PyVal::Str(v),
        None => PyVal::None,
    })
}

/// `oe.types.boolean` (OE-core `lib/oe/types.py`).
fn oe_boolean(v: &PyVal) -> Result<bool, Stop> {
    match v {
        PyVal::None => Ok(false),
        PyVal::Bool(b) => Ok(*b),
        PyVal::Str(s) => {
            if !s.is_ascii() {
                return Err(unsupported("oe.types.boolean of non-ASCII text"));
            }
            match s.to_ascii_lowercase().as_str() {
                "yes" | "y" | "true" | "t" | "1" => Ok(true),
                "no" | "n" | "false" | "f" | "0" => Ok(false),
                _ => Err(unsupported("oe.types.boolean raises ValueError")),
            }
        }
        other => Err(unsupported(format!(
            "oe.types.boolean of {} (a TypeError)",
            other.type_name()
        ))),
    }
}

/// `float(x)` for the plain decimal forms only (`12`, `1.5`); anything else is not
/// reproduced.
fn py_float(v: &PyVal) -> Result<f64, Stop> {
    match v {
        PyVal::Int(i) if i.unsigned_abs() < (1u128 << 53) => Ok(*i as f64),
        PyVal::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        PyVal::Str(s) => {
            let (int, frac) = match s.split_once('.') {
                Some((a, b)) => (a, Some(b)),
                None => (s.as_str(), None),
            };
            let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
            if !digits(int) || frac.is_some_and(|f| !digits(f)) || s.len() > 30 {
                return Err(unsupported("float() of that text"));
            }
            s.parse::<f64>()
                .map_err(|_| unsupported("float() of that text"))
        }
        other => Err(unsupported(format!("float() of {}", other.type_name()))),
    }
}

fn call(func: Func, a: Vec<PyVal>, env: &mut dyn Env) -> Result<PyVal, Stop> {
    match func {
        Func::BbContains | Func::BbContainsAny => {
            let val = get_var(env, &a[0])?;
            contains(func == Func::BbContains, &val, &a[1], &a[2], &a[3])
        }
        Func::OeAllDistroFeatures | Func::OeAnyDistroFeatures => {
            let val = get_var(env, &PyVal::Str("DISTRO_FEATURES".into()))?;
            contains(func == Func::OeAllDistroFeatures, &val, &a[1], &a[2], &a[3])
        }
        Func::BbFilter => {
            // `filter_string(d.getVar(variable), checkvalues)`.
            let val = get_var(env, &a[0])?;
            if !val.truthy()? {
                return Ok(PyVal::Str(String::new()));
            }
            let words = py_split_ws(want_str(&val, "the value")?);
            let check = check_set(&a[1])?;
            let mut hit: Vec<String> = check
                .into_iter()
                .filter_map(|c| match c {
                    PyVal::Str(s) if words.contains(&s) => Some(s),
                    _ => None,
                })
                .collect();
            hit.sort();
            Ok(PyVal::Str(hit.join(" ")))
        }
        Func::BbToBoolean => {
            let s = &a[0];
            if !s.truthy()? {
                return Ok(a[1].clone());
            }
            match s {
                PyVal::Int(i) => Ok(PyVal::Bool(*i != 0)),
                PyVal::Bool(b) => Ok(PyVal::Bool(*b)),
                PyVal::Str(t) => {
                    if !t.is_ascii() {
                        return Err(unsupported("to_boolean of non-ASCII text"));
                    }
                    match t.to_ascii_lowercase().as_str() {
                        "y" | "yes" | "1" | "true" => Ok(PyVal::Bool(true)),
                        "n" | "no" | "0" | "false" => Ok(PyVal::Bool(false)),
                        _ => Err(unsupported("to_boolean raises ValueError")),
                    }
                }
                other => Err(unsupported(format!("to_boolean of {}", other.type_name()))),
            }
        }
        Func::BbVercmpString => {
            let x = want_str(&a[0], "a version")?;
            let y = want_str(&a[1], "a version")?;
            Ok(PyVal::Int(vercmp_string(x, y)?))
        }
        Func::OeConditional => {
            let val = get_var(env, &a[0])?;
            Ok(if py_eq(&val, &a[1])? {
                a[2].clone()
            } else {
                a[3].clone()
            })
        }
        Func::OeIfelse => Ok(if a[0].truthy()? {
            a[1].clone()
        } else {
            a[2].clone()
        }),
        Func::OeLessOrEqual => {
            let val = get_var(env, &a[0])?;
            Ok(if py_float(&val)? <= py_float(&a[1])? {
                a[2].clone()
            } else {
                a[3].clone()
            })
        }
        Func::OeVersionLessOrEqual => {
            let val = get_var(env, &a[0])?;
            let x = want_str(&val, "the value")?;
            let y = want_str(&a[1], "the version")?;
            Ok(if vercmp_string(x, y)? <= 0 {
                a[2].clone()
            } else {
                a[3].clone()
            })
        }
        Func::OeBothContain => {
            let v1 = get_var(env, &a[0])?;
            let v2 = get_var(env, &a[1])?;
            let w1 = py_split_ws(want_str(&v1, "the first value")?);
            let w2 = py_split_ws(want_str(&v2, "the second value")?);
            let check = check_set(&a[2])?;
            let subset = check
                .iter()
                .all(|c| matches!(c, PyVal::Str(s) if w1.contains(s) && w2.contains(s)));
            if !subset {
                return Ok(PyVal::Str(String::new()));
            }
            match check.as_slice() {
                [] => Ok(PyVal::Str(String::new())),
                [PyVal::Str(one)] => Ok(PyVal::Str(one.clone())),
                _ => Err(unsupported(
                    "both_contain of several words (python set order)",
                )),
            }
        }
        Func::OePruneSuffix => {
            let mut var: Vec<char> = want_str(&a[0], "the name")?.chars().collect();
            let suffixes: Vec<PyVal> = match &a[1] {
                PyVal::List(l) => l.clone(),
                PyVal::Str(s) => s.chars().map(|c| PyVal::Str(c.to_string())).collect(),
                other => {
                    return Err(unsupported(format!(
                        "prune_suffix suffixes of {}",
                        other.type_name()
                    )));
                }
            };
            for suf in &suffixes {
                if !suf.truthy()? {
                    continue;
                }
                let suf: Vec<char> = want_str(suf, "a suffix")?.chars().collect();
                if var.ends_with(&suf) {
                    var.truncate(var.len() - suf.len());
                }
            }
            let prefix = get_var(env, &PyVal::Str("MLPREFIX".into()))?;
            if prefix.truthy()? {
                let p: Vec<char> = want_str(&prefix, "MLPREFIX")?.chars().collect();
                if var.starts_with(&p) {
                    var.drain(..p.len());
                }
            }
            Ok(PyVal::Str(var.into_iter().collect()))
        }
        Func::OeVartrue => {
            let val = get_var(env, &a[0])?;
            Ok(if oe_boolean(&val)? {
                a[1].clone()
            } else {
                a[2].clone()
            })
        }
        Func::OeSetDifference => set_difference(env, &a[0], &a[1]),
        Func::OeFilterDefaultFeatures => {
            let n = want_str(&a[0], "the variable name")?;
            set_difference(
                env,
                &PyVal::Str(format!("{n}_DEFAULTS")),
                &PyVal::Str(format!("{n}_OPTED_OUT")),
            )
        }
        Func::DGetVar => {
            if !a[1].truthy()? {
                return Err(unsupported("d.getVar with expand=False"));
            }
            get_var(env, &a[0])
        }
        Func::DGetVarFlag => {
            if !a[2].truthy()? {
                return Err(unsupported("d.getVarFlag with expand=False"));
            }
            let n = want_str(&a[0], "the variable name")?;
            let f = want_str(&a[1], "the flag name")?;
            if f.starts_with('_') {
                return Err(unsupported("an internal flag"));
            }
            Ok(match env.get_var_flag(n, f)? {
                Some(v) => PyVal::Str(v),
                None => PyVal::None,
            })
        }
        Func::DExpand => {
            let s = want_str(&a[0], "the text")?;
            Ok(PyVal::Str(env.expand(s)?))
        }
        Func::Len => match &a[0] {
            PyVal::Str(s) => Ok(PyVal::Int(s.chars().count() as i128)),
            PyVal::List(l) => Ok(PyVal::Int(l.len() as i128)),
            other => Err(unsupported(format!("len() of {}", other.type_name()))),
        },
    }
}

/// `bb.utils.contains` (`all`) / `contains_any` on the value read.
fn contains(
    all: bool,
    val: &PyVal,
    checkvalues: &PyVal,
    truevalue: &PyVal,
    falsevalue: &PyVal,
) -> Result<PyVal, Stop> {
    if !val.truthy()? {
        return Ok(falsevalue.clone());
    }
    let words = py_split_ws(want_str(val, "the value")?);
    let check = check_set(checkvalues)?;
    let has = |c: &PyVal| matches!(c, PyVal::Str(s) if words.contains(s));
    let hit = if all {
        check.iter().all(has)
    } else {
        check.iter().any(has)
    };
    Ok(if hit {
        truevalue.clone()
    } else {
        falsevalue.clone()
    })
}

/// `oe.utils.set_difference` (OE-core Wrynose `lib/oe/utils.py`).
fn set_difference(env: &mut dyn Env, v1: &PyVal, v2: &PyVal) -> Result<PyVal, Stop> {
    let val1 = get_var(env, v1)?;
    if !val1.truthy()? {
        return Ok(PyVal::Str(String::new()));
    }
    let val2 = get_var(env, v2)?;
    if !val2.truthy()? {
        return Ok(val1);
    }
    let w1 = py_split_ws(want_str(&val1, "the first value")?);
    let w2 = py_split_ws(want_str(&val2, "the second value")?);
    if w2.iter().any(|w| w == "*") {
        return Ok(PyVal::Str(String::new()));
    }
    let mut out: Vec<String> = Vec::new();
    for w in w1 {
        if !w2.contains(&w) && !out.contains(&w) {
            out.push(w);
        }
    }
    out.sort();
    Ok(PyVal::Str(out.join(" ")))
}

// ---------------------------------------------------------------------------------------
// bb.utils.vercmp_string (BitBake `lib/bb/utils.py`: explode_version, split_version,
// vercmp_part, vercmp).

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    /// `(0, int)`: the digits without leading zeros (compared by length, then text).
    Num(String),
    /// `(1, letters)`.
    Alpha(String),
    /// `(-1, '~')`.
    Tilde,
    /// `(2, char)`.
    Other(char),
}

impl Part {
    fn order(&self) -> i32 {
        match self {
            Part::Tilde => -1,
            Part::Num(_) => 0,
            Part::Alpha(_) => 1,
            Part::Other(_) => 2,
        }
    }
}

fn explode_version(s: &str) -> Vec<Part> {
    let c: Vec<char> = s.chars().collect();
    let mut r = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if c[i].is_ascii_digit() {
            let mut j = i;
            while j < c.len() && c[j].is_ascii_digit() {
                j += 1;
            }
            let digits: String = c[i..j].iter().collect();
            let trimmed = digits.trim_start_matches('0');
            r.push(Part::Num(if trimmed.is_empty() {
                "0".into()
            } else {
                trimmed.to_string()
            }));
            i = j;
        } else if c[i].is_ascii_alphabetic() {
            let mut j = i;
            while j < c.len() && c[j].is_ascii_alphabetic() {
                j += 1;
            }
            r.push(Part::Alpha(c[i..j].iter().collect()));
            i = j;
        } else {
            r.push(if c[i] == '~' {
                Part::Tilde
            } else {
                Part::Other(c[i])
            });
            i += 1;
        }
    }
    r
}

fn cmp_num(a: &str, b: &str) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn vercmp_part(a: &str, b: &str) -> i128 {
    let va = explode_version(a);
    let vb = explode_version(b);
    let mut k = 0;
    loop {
        let pa = va.get(k);
        let pb = vb.get(k);
        k += 1;
        // An exhausted side is `(0, None)`.
        let (oa, ob) = (pa.map_or(0, Part::order), pb.map_or(0, Part::order));
        match (pa, pb) {
            (None, None) => return 0,
            _ if oa < ob => return -1,
            _ if oa > ob => return 1,
            (None, _) => return -1,
            (_, None) => return 1,
            (Some(x), Some(y)) => {
                let o = match (x, y) {
                    (Part::Num(p), Part::Num(q)) => cmp_num(p, q),
                    (Part::Alpha(p), Part::Alpha(q)) => p.cmp(q),
                    (Part::Other(p), Part::Other(q)) => p.cmp(q),
                    _ => Ordering::Equal,
                };
                match o {
                    Ordering::Less => return -1,
                    Ordering::Greater => return 1,
                    Ordering::Equal => {}
                }
            }
        }
    }
}

/// `split_version`: `(epoch, version, revision)`.
fn split_version(s: &str) -> Result<(i128, String, String), Stop> {
    let s = s.trim_matches(|c| c == ' ' || c == '<' || c == '>' || c == '=');
    let mut v = s.to_string();
    let mut e = 0;
    if v.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        let ep = parts[0];
        if ep.is_empty() || ep.len() > 30 || !ep.bytes().all(|b| b.is_ascii_digit()) {
            return Err(unsupported("int() of that epoch"));
        }
        e = ep
            .parse::<i128>()
            .map_err(|_| unsupported("int() of that epoch"))?;
        v = parts.get(1).copied().unwrap_or("").to_string();
    }
    let mut r = String::new();
    if let Some((head, tail)) = v.rsplit_once('-') {
        r = tail.to_string();
        v = head.to_string();
    }
    Ok((e, v, r))
}

/// `bb.utils.vercmp_string(a, b)`. Versions with characters other than printable ASCII are
/// not reproduced (python's `\d` and `$` would differ from this scan there).
pub fn vercmp_string(a: &str, b: &str) -> Result<i128, Stop> {
    let ok = |s: &str| s.chars().all(|c| c.is_ascii() && !c.is_ascii_control());
    if !ok(a) || !ok(b) {
        return Err(unsupported(
            "a version with non-ASCII or control characters",
        ));
    }
    let (ea, va, ra) = split_version(a)?;
    let (eb, vb, rb) = split_version(b)?;
    let mut r = ea - eb;
    if r == 0 {
        r = vercmp_part(&va, &vb);
    }
    if r == 0 {
        r = vercmp_part(&ra, &rb);
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Vars {
        vars: HashMap<String, String>,
        flags: HashMap<(String, String), String>,
        uncertain: Vec<String>,
        reads: Vec<String>,
    }

    impl Env for Vars {
        fn get_var(&mut self, name: &str) -> Result<Option<String>, Stop> {
            self.reads.push(name.to_string());
            if self.uncertain.iter().any(|u| u == name) {
                return Err(Stop::Uncertain(name.to_string()));
            }
            Ok(self.vars.get(name).cloned())
        }
        fn get_var_flag(&mut self, name: &str, flag: &str) -> Result<Option<String>, Stop> {
            Ok(self
                .flags
                .get(&(name.to_string(), flag.to_string()))
                .cloned())
        }
        fn expand(&mut self, text: &str) -> Result<String, Stop> {
            Ok(text.replace("${A}", "a"))
        }
    }

    fn env(pairs: &[(&str, &str)]) -> Vars {
        Vars {
            vars: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Vars::default()
        }
    }

    fn run(code: &str, e: &mut Vars) -> Result<String, Stop> {
        let x = parse(code)?;
        to_text(&eval(&x, e)?)
    }

    fn ok(code: &str, pairs: &[(&str, &str)]) -> String {
        match run(code, &mut env(pairs)) {
            Ok(s) => s,
            Err(e) => panic!("{code}: {e:?}"),
        }
    }

    fn not_evaluated(code: &str, pairs: &[(&str, &str)]) {
        assert!(run(code, &mut env(pairs)).is_err(), "{code} was evaluated");
    }

    #[test]
    fn accepts_the_subset() {
        let v = &[("X", "a b c"), ("E", ""), ("PV", "1.2.3")];
        assert_eq!(ok("'x'", v), "x");
        assert_eq!(ok("\"x\"", v), "x");
        assert_eq!(ok("  'x'  ", v), "x");
        assert_eq!(ok("'a' + 'b'", v), "ab");
        assert_eq!(ok("12", v), "12");
        assert_eq!(ok("True", v), "True");
        assert_eq!(ok("None", v), "None");
        assert_eq!(ok("'y' if d.getVar('X') else 'n'", v), "y");
        assert_eq!(ok("'y' if d.getVar('E') else 'n'", v), "n");
        assert_eq!(ok("d.getVar('E') or 'dflt'", v), "dflt");
        assert_eq!(ok("d.getVar('X') and 'k'", v), "k");
        assert_eq!(ok("d.getVar('E') and 'k'", v), "");
        assert_eq!(ok("not d.getVar('E')", v), "True");
        assert_eq!(ok("d.getVar('X') == 'a b c'", v), "True");
        assert_eq!(ok("d.getVar('X') != 'a b c'", v), "False");
        assert_eq!(ok("'b' in d.getVar('X')", v), "True");
        assert_eq!(ok("'b' in d.getVar('X').split()", v), "True");
        assert_eq!(ok("'z' not in d.getVar('X').split()", v), "True");
        assert_eq!(ok("['', 'x:']['x' != '']", v), "x:");
        assert_eq!(ok("['', 'x:'][d.getVar('E') != '']", v), "");
        assert_eq!(ok("d.getVar('PV').split('.')[0]", v), "1");
        assert_eq!(ok("d.getVar('PV').split('.')[-1]", v), "3");
        assert_eq!(ok("'-'.join(d.getVar('X').split())", v), "a-b-c");
        assert_eq!(ok("d.getVar('X').replace(' ', ',')", v), "a,b,c");
        assert_eq!(ok("' x '.strip()", v), "x");
        assert_eq!(ok("'xxaxx'.strip('x')", v), "a");
        assert_eq!(ok("'Ab'.lower() + 'Ab'.upper()", v), "abAB");
        assert_eq!(ok("'abc'.startswith('ab')", v), "True");
        assert_eq!(ok("'abc'.endswith('ab')", v), "False");
        assert_eq!(ok("len('abc')", v), "3");
        assert_eq!(ok("d.getVar('UNSET')", v), "None");
        assert_eq!(ok("d.getVar('X', True)", v), "a b c");
        assert_eq!(ok("d.getVar('X', 1)", v), "a b c");
        assert_eq!(ok("d.getVar('X', expand=True)", v), "a b c");
        assert_eq!(ok("d.expand('${A}-b')", v), "a-b");
        assert_eq!(ok("'a\\'b\\\\c\\n'", v), "a'b\\c\n");
        assert_eq!(ok("('x')", v), "x");
        assert_eq!(ok("'a' if 'b' else 'c' if 1 else 'd'", v), "a");
        assert_eq!(ok("bb.utils.contains('X', 'a', 'y', 'n', d,)", v), "y");
    }

    #[test]
    fn rejects_everything_else() {
        let v = &[("X", "a b c")];
        for code in [
            "f'x'",
            "b'x'",
            "r'x'",
            "u'x'",
            "'''x'''",
            "'a' 'b'",
            "'\\x41'",
            "'\\u0041'",
            "1.5",
            "0x10",
            "1_000",
            "01",
            "x",
            "PN",
            "bb.utils.which(d.getVar('PATH'), 'x')",
            "oe.utils.cpu_count()",
            "oe.utils.get_multilib_datastore('x', d)",
            "bb.parse.vars_from_file(d.getVar('FILE', False), d)[0]",
            "os.uname()[4]",
            "d.getVar('FILE', False)",
            "d.getVar('X', expand=False)",
            "d.getVar('X', noweakdefault=True)",
            "d.setVar('X', 'y')",
            "d.appendVar('X', 'y')",
            "d",
            "str(d)",
            "d.getVar",
            "d.getVar('X').split",
            "d.getVar('X').upper",
            "'%s' % 'x'",
            "'a' * 2",
            "1 - 1",
            "-d.getVar('X')",
            "[x for x in 'ab']",
            "(lambda: 1)()",
            "'a' < 'b'",
            "'a' == 'a' == 'a'",
            "'a' is 'a'",
            "('a', 'b')",
            "()",
            "{'a': 1}",
            "d.getVar('X')[0:2]",
            "'abc'[0]",
            "d.getVar('X').split(',', 1)",
            "d.getVar('X').rsplit()",
            "d.getVar('X').format()",
            "d.getVar('X').replace('a', 'b', 1)",
            "'a'.startswith(('a', 'b'))",
            "x if y",
            "'a' if True",
            "'a' if 'b' if 1 else 'c' else 'd'",
            "'it' 's'",
            "foo(d)",
            "len",
            "bb.utils",
            "bb.utils.contains",
            "bb.data.inherits_class('native', d)",
            "oe.types.boolean('1')",
            "oe.utils.set_intersect('A', 'B', d)",
            "bb.utils.contains('X', 'a', 'y', 'n', localdata)",
            "bb.utils.contains('X', 'a', 'y', 'n')",
            "bb.utils.contains('X', 'a', 'y', 'n', d, 1)",
            "bb.utils.contains(variable='X', 'a', 'y', 'n', d)",
            "bb.utils.contains('X', checkvalues='a', checkvalues='b', truevalue='y', falsevalue='n', d=d)",
            "bb.utils.contains('X', 'a', 'y', 'n', d=d, extra=1)",
            "'a';'b'",
            "'a' # comment",
            "import os",
            "x = 1",
            "'é'.upper()",
            "",
            "   ",
        ] {
            not_evaluated(code, v);
        }
    }

    #[test]
    fn python_errors_are_not_evaluated() {
        let v = &[("X", "a b"), ("B", "maybe")];
        for code in [
            "d.getVar('UNSET').split()",
            "d.getVar('X').split()[5]",
            "d.getVar('X') + 1",
            "'a' + None",
            "1 in 'abc'",
            "bb.utils.to_boolean(d.getVar('B'))",
            "oe.utils.vartrue('B', 'y', 'n', d)",
            "str([1])",
            "['a', 'b']",
            "d.getVar('X').split('')",
            "oe.utils.less_or_equal('X', '1', 'a', 'b', d)",
            "oe.utils.version_less_or_equal('UNSET', '1', 'a', 'b', d)",
            "oe.utils.both_contain('X', 'UNSET', 'a', d)",
        ] {
            not_evaluated(code, v);
        }
    }

    #[test]
    fn uncertain_reads_stop_and_name_the_variable() {
        let mut e = env(&[("X", "a")]);
        e.uncertain.push("DISTRO_FEATURES".into());
        let r = run(
            "bb.utils.contains('DISTRO_FEATURES', 'x11', 'a', 'b', d)",
            &mut e,
        );
        assert_eq!(r, Err(Stop::Uncertain("DISTRO_FEATURES".into())));
        // Only the branch python takes is evaluated.
        let r = run("d.getVar('X') or d.getVar('DISTRO_FEATURES')", &mut e);
        assert_eq!(r, Ok("a".into()));
        assert_eq!(e.reads.last().map(String::as_str), Some("X"));
    }

    #[test]
    fn unterminated_strings() {
        assert!(has_unterminated_string("'abc"));
        assert!(has_unterminated_string("bb.utils.contains('A', 'b"));
        assert!(has_unterminated_string("'a\\'"));
        assert!(has_unterminated_string("'''abc''"));
        assert!(!has_unterminated_string("'a\\'b'"));
        assert!(!has_unterminated_string("'a' + \"b\""));
        assert!(!has_unterminated_string("x # 'abc"));
        assert!(!has_unterminated_string("'''a'''"));
        assert!(!has_unterminated_string(""));
    }

    // bb.utils.contains (BitBake 2.18 lib/bb/utils.py): `if not val: return falsevalue`;
    // `checkvalues` split if a string, else `set()`; `issubset`.
    #[test]
    fn bb_contains() {
        let v = &[("F", "x11  wayland\topengl"), ("E", "")];
        assert_eq!(ok("bb.utils.contains('F', 'x11', 'y', 'n', d)", v), "y");
        assert_eq!(
            ok("bb.utils.contains('F', 'x11 opengl', 'y', 'n', d)", v),
            "y"
        );
        assert_eq!(
            ok("bb.utils.contains('F', 'x11 vulkan', 'y', 'n', d)", v),
            "n"
        );
        assert_eq!(
            ok("bb.utils.contains('F', ['x11', 'wayland'], 'y', 'n', d)", v),
            "y"
        );
        assert_eq!(ok("bb.utils.contains('F', 'x1', 'y', 'n', d)", v), "n");
        // empty checkvalues: the empty set is a subset
        assert_eq!(ok("bb.utils.contains('F', '', 'y', 'n', d)", v), "y");
        assert_eq!(ok("bb.utils.contains('F', [], 'y', 'n', d)", v), "y");
        // unset or empty value: falsevalue, even for empty checkvalues
        assert_eq!(ok("bb.utils.contains('E', '', 'y', 'n', d)", v), "n");
        assert_eq!(ok("bb.utils.contains('UNSET', 'x', 'y', 'n', d)", v), "n");
        // values are returned as given: str(True)
        assert_eq!(
            ok("bb.utils.contains('F', 'x11', True, False, d)", v),
            "True"
        );
        assert_eq!(
            ok(
                "bb.utils.contains(variable='F', checkvalues='x11', truevalue='y', falsevalue='n', d=d)",
                v
            ),
            "y"
        );
        // a non-string element is never in the value
        assert_eq!(ok("bb.utils.contains('F', [1], 'y', 'n', d)", v), "n");
        not_evaluated("bb.utils.contains('F', None, 'y', 'n', d)", v);
        not_evaluated("bb.utils.contains('F', 1, 'y', 'n', d)", v);
    }

    #[test]
    fn bb_contains_any() {
        let v = &[("F", "x11 wayland"), ("E", "")];
        assert_eq!(
            ok("bb.utils.contains_any('F', 'vulkan x11', 'y', 'n', d)", v),
            "y"
        );
        assert_eq!(
            ok("bb.utils.contains_any('F', 'vulkan', 'y', 'n', d)", v),
            "n"
        );
        assert_eq!(ok("bb.utils.contains_any('F', '', 'y', 'n', d)", v), "n");
        assert_eq!(ok("bb.utils.contains_any('E', 'x11', 'y', 'n', d)", v), "n");
        assert_eq!(
            ok("bb.utils.contains_any('UNSET', 'x11', 'y', 'n', d)", v),
            "n"
        );
    }

    // filter_string: `if not val: return ''`; `' '.join(sorted(checkvalues & val))`.
    #[test]
    fn bb_filter() {
        let v = &[("P", "zstd gnutls openssl ipv6"), ("E", "")];
        assert_eq!(
            ok("bb.utils.filter('P', 'openssl gnutls mbedtls', d)", v),
            "gnutls openssl"
        );
        assert_eq!(ok("bb.utils.filter('P', 'mbedtls', d)", v), "");
        assert_eq!(ok("bb.utils.filter('E', 'x', d)", v), "");
        assert_eq!(ok("bb.utils.filter('UNSET', 'x', d)", v), "");
        assert_eq!(ok("bb.utils.filter('P', 'zstd zstd', d)", v), "zstd");
        assert_eq!(
            ok(
                "'--without-ssl' if (bb.utils.filter('P', 'mbedtls', d) == '') else ''",
                v
            ),
            "--without-ssl"
        );
    }

    #[test]
    fn bb_to_boolean() {
        let v = &[("Y", "Yes"), ("N", "0"), ("E", ""), ("B", "maybe")];
        assert_eq!(ok("bb.utils.to_boolean(d.getVar('Y'))", v), "True");
        assert_eq!(ok("bb.utils.to_boolean(d.getVar('N'))", v), "False");
        assert_eq!(ok("bb.utils.to_boolean(d.getVar('E'))", v), "None");
        assert_eq!(
            ok("bb.utils.to_boolean(d.getVar('UNSET'), True)", v),
            "True"
        );
        assert_eq!(
            ok("bb.utils.to_boolean(d.getVar('E'), default=False)", v),
            "False"
        );
        assert_eq!(ok("bb.utils.to_boolean(2)", v), "True");
        assert_eq!(ok("bb.utils.to_boolean(0, 'd')", v), "d");
        assert_eq!(ok("bb.utils.to_boolean(True)", v), "True");
        assert_eq!(ok("bb.utils.to_boolean('TRUE')", v), "True");
        assert_eq!(ok("bb.utils.to_boolean('n')", v), "False");
        not_evaluated("bb.utils.to_boolean(d.getVar('B'))", v);
        not_evaluated("bb.utils.to_boolean('t')", v);
    }

    // vercmp_string: split_version strips " <>=", epoch before ':', revision after the last
    // '-'; explode_version / vercmp_part.
    #[test]
    fn bb_vercmp_string() {
        let c = |a: &str, b: &str| vercmp_string(a, b).ok();
        assert_eq!(c("1.0", "1.0"), Some(0));
        assert_eq!(c("1.0", "1.1"), Some(-1));
        assert_eq!(c("1.10", "1.9"), Some(1));
        assert_eq!(c("1.0", "1.0.1"), Some(-1));
        assert_eq!(c("1.0~rc1", "1.0"), Some(-1));
        assert_eq!(c("1.0a", "1.0"), Some(1));
        assert_eq!(c("1.0a", "1.0b"), Some(-1));
        assert_eq!(c("1.0a", "1.0.1"), Some(-1));
        assert_eq!(c("2:1.0", "1:9.0"), Some(1));
        assert_eq!(c("3:1.0", "1:9.0"), Some(2));
        assert_eq!(c("1.0-r1", "1.0-r0"), Some(1));
        assert_eq!(c(">=1.0", "1.0"), Some(0));
        assert_eq!(c("007", "7"), Some(0));
        assert_eq!(c("1.0", "1.0-r0"), Some(-1));
        assert_eq!(c("x:1", "1"), None);
        assert_eq!(ok("bb.utils.vercmp_string('1.2', '1.10')", &[]), "-1");
    }

    #[test]
    fn oe_conditional_ifelse_vartrue() {
        let v = &[("I", "systemd"), ("D", "1"), ("B", "maybe")];
        assert_eq!(
            ok("oe.utils.conditional('I', 'systemd', 'pam', '', d)", v),
            "pam"
        );
        assert_eq!(
            ok("oe.utils.conditional('I', 'sysvinit', 'pam', 'no', d)", v),
            "no"
        );
        assert_eq!(ok("oe.utils.conditional('UNSET', '', 'a', 'b', d)", v), "b");
        assert_eq!(ok("oe.utils.ifelse(d.getVar('I'))", v), "True");
        assert_eq!(ok("oe.utils.ifelse(d.getVar('UNSET'), 'a')", v), "False");
        assert_eq!(ok("oe.utils.ifelse('', iffalse='z')", v), "z");
        assert_eq!(ok("oe.utils.vartrue('D', 'a', 'b', d)", v), "a");
        assert_eq!(ok("oe.utils.vartrue('UNSET', 'a', 'b', d)", v), "b");
        assert_eq!(ok("oe.utils.vartrue('I', 'a', 'b', d)", &[("I", "T")]), "a");
        not_evaluated("oe.utils.vartrue('B', 'a', 'b', d)", v);
    }

    #[test]
    fn oe_less_or_equal() {
        let v = &[("V", "4.9"), ("N", "10")];
        assert_eq!(ok("oe.utils.less_or_equal('V', '5', 'a', 'b', d)", v), "a");
        assert_eq!(
            ok("oe.utils.less_or_equal('N', '9.5', 'a', 'b', d)", v),
            "b"
        );
        assert_eq!(ok("oe.utils.less_or_equal('N', 10, 'a', 'b', d)", v), "a");
        not_evaluated("oe.utils.less_or_equal('UNSET', '5', 'a', 'b', d)", v);
        not_evaluated("oe.utils.less_or_equal('V', '1e3', 'a', 'b', d)", v);
        assert_eq!(
            ok(
                "oe.utils.version_less_or_equal('V', '4.10', 'a', 'b', d)",
                v
            ),
            "a"
        );
        assert_eq!(
            ok("oe.utils.version_less_or_equal('N', '9', 'a', 'b', d)", v),
            "b"
        );
    }

    #[test]
    fn oe_both_contain() {
        let v = &[("A", "x y z"), ("B", "y z w")];
        assert_eq!(ok("oe.utils.both_contain('A', 'B', 'y', d)", v), "y");
        assert_eq!(ok("oe.utils.both_contain('A', 'B', 'x', d)", v), "");
        assert_eq!(ok("oe.utils.both_contain('A', 'B', 'x y', d)", v), "");
        assert_eq!(ok("oe.utils.both_contain('A', 'B', '', d)", v), "");
        // two words in both: the result's order is python's set order
        not_evaluated("oe.utils.both_contain('A', 'B', 'y z', d)", v);
        not_evaluated("oe.utils.both_contain('A', 'UNSET', 'y', d)", v);
    }

    #[test]
    fn oe_prune_suffix() {
        let v = &[("PN", "lib32-zlib-native"), ("MLPREFIX", "lib32-")];
        assert_eq!(
            ok(
                "oe.utils.prune_suffix(d.getVar('PN'), ['-native', '-cross'], d)",
                v
            ),
            "zlib"
        );
        assert_eq!(
            ok(
                "oe.utils.prune_suffix(d.getVar('PN'), 'x -native -cross'.split(), d)",
                &[("PN", "zlib-native")]
            ),
            "zlib"
        );
        // each suffix in order, on what the previous ones left
        assert_eq!(
            ok("oe.utils.prune_suffix('a-b-c', ['-c', '-b', ''], d)", &[]),
            "a"
        );
        assert_eq!(ok("oe.utils.prune_suffix('a-b', ['-x'], d)", &[]), "a-b");
    }

    #[test]
    fn oe_distro_features() {
        let v = &[("DISTRO_FEATURES", "systemd pam x11")];
        assert_eq!(ok("oe.utils.all_distro_features(d, 'pam x11')", v), "1");
        assert_eq!(ok("oe.utils.all_distro_features(d, 'pam wayland')", v), "");
        assert_eq!(
            ok(
                "oe.utils.all_distro_features(d, 'pam', 'yes', falsevalue='no')",
                v
            ),
            "yes"
        );
        assert_eq!(ok("oe.utils.any_distro_features(d, 'wayland x11')", v), "1");
        assert_eq!(ok("oe.utils.any_distro_features(d, 'wayland')", v), "");
    }

    // set_difference / filter_default_features: OE-core Wrynose only.
    #[test]
    fn oe_set_difference() {
        let v = &[
            ("DISTRO_FEATURES_DEFAULTS", "x11 acl ipv6 acl"),
            ("DISTRO_FEATURES_OPTED_OUT", "x11"),
            ("MACHINE_FEATURES_DEFAULTS", "rtc qemu-usermode"),
            ("STAR_DEFAULTS", "a"),
            ("STAR_OPTED_OUT", "b *"),
        ];
        assert_eq!(
            ok("oe.utils.filter_default_features('DISTRO_FEATURES', d)", v),
            "acl ipv6"
        );
        // second operand unset: the first value as it is
        assert_eq!(
            ok("oe.utils.filter_default_features('MACHINE_FEATURES', d)", v),
            "rtc qemu-usermode"
        );
        assert_eq!(ok("oe.utils.filter_default_features('STAR', d)", v), "");
        assert_eq!(ok("oe.utils.filter_default_features('UNSET', d)", v), "");
    }

    #[test]
    fn get_var_flag() {
        let mut e = env(&[]);
        e.flags
            .insert(("TUNEFILE".into(), "armv7".into()), "x.inc".into());
        assert_eq!(
            run("d.getVarFlag('TUNEFILE', 'armv7')", &mut e),
            Ok("x.inc".into())
        );
        assert_eq!(
            run("d.getVarFlag('TUNEFILE', 'other')", &mut e),
            Ok("None".into())
        );
        assert!(run("d.getVarFlag('TUNEFILE', 'armv7', False)", &mut e).is_err());
    }

    #[test]
    fn funcs_are_listed() {
        let e = match parse("bb.utils.contains('A', 'b', d.getVar('X'), '', d) + len('x')") {
            Ok(e) => e,
            Err(e) => panic!("{e:?}"),
        };
        assert_eq!(e.funcs(), vec![Func::BbContains, Func::DGetVar, Func::Len]);
    }
}

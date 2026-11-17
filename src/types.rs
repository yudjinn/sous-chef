//! Shared types used across every module. See `docs/architecture.md` ("Shared types").

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Serialize;

use crate::error::Error;

/// A range of lines, 1-based and inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct LineSpan {
    pub start: u32,
    pub end: u32,
}

impl LineSpan {
    pub fn new(start: u32, end: u32) -> LineSpan {
        LineSpan { start, end }
    }

    /// A span covering a single line.
    pub fn line(n: u32) -> LineSpan {
        LineSpan { start: n, end: n }
    }
}

impl fmt::Display for LineSpan {
    /// `A-B`, or `A` when the span is a single line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.start == self.end {
            write!(f, "{}", self.start)
        } else {
            write!(f, "{}-{}", self.start, self.end)
        }
    }
}

impl FromStr for LineSpan {
    type Err = Error;

    /// Parses `A` or `A-B` (1-based, `A <= B`).
    fn from_str(s: &str) -> Result<LineSpan, Error> {
        let bad = || Error::Invalid {
            msg: format!("bad line range {s:?}: expected A or A-B with 1 <= A <= B"),
        };
        let (a, b) = match s.split_once('-') {
            Some((a, b)) => (a, b),
            None => (s, s),
        };
        let start: u32 = a.trim().parse().map_err(|_| bad())?;
        let end: u32 = b.trim().parse().map_err(|_| bad())?;
        if start == 0 || end < start {
            return Err(bad());
        }
        Ok(LineSpan { start, end })
    }
}

/// A pointer. `path` is relative to the layer root.
///
/// Display and parse form: `layer:path:A-B` (`layer:path:A` when `A == B`). The layer name
/// never contains `:`; the path may contain anything except that the last `:` separates the
/// line range.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Loc {
    pub layer: String,
    pub path: PathBuf,
    pub lines: LineSpan,
}

impl fmt::Display for Loc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.layer, self.path.display(), self.lines)
    }
}

impl FromStr for Loc {
    type Err = Error;

    fn from_str(s: &str) -> Result<Loc, Error> {
        let bad = |why: &str| Error::Invalid {
            msg: format!("bad pointer {s:?}: {why}; expected layer:path:A-B"),
        };
        let (layer, rest) = s.split_once(':').ok_or_else(|| bad("missing ':'"))?;
        let (path, lines) = rest
            .rsplit_once(':')
            .ok_or_else(|| bad("missing line range"))?;
        if layer.is_empty() {
            return Err(bad("empty layer name"));
        }
        if path.is_empty() {
            return Err(bad("empty path"));
        }
        let lines: LineSpan = lines.parse().map_err(|_| bad("bad line range"))?;
        Ok(Loc {
            layer: layer.to_string(),
            path: PathBuf::from(path),
            lines,
        })
    }
}

/// Kind of a BitBake metadata file, by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum FileKind {
    /// `.bb`
    Recipe,
    /// `.bbappend`
    Append,
    /// `.inc`
    Include,
    /// `.bbclass`
    Class,
    /// `.conf`
    Conf,
}

impl FileKind {
    /// Classifies a path by its extension. `None` for anything that is not BitBake metadata.
    pub fn from_path(path: &Path) -> Option<FileKind> {
        match path.extension()?.to_str()? {
            "bb" => Some(FileKind::Recipe),
            "bbappend" => Some(FileKind::Append),
            "inc" => Some(FileKind::Include),
            "bbclass" => Some(FileKind::Class),
            "conf" => Some(FileKind::Conf),
            _ => None,
        }
    }

    /// The text stored in the index `files.kind` column: `bb`, `bbappend`, `inc`, `bbclass`,
    /// `conf` (the file extension).
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Recipe => "bb",
            FileKind::Append => "bbappend",
            FileKind::Include => "inc",
            FileKind::Class => "bbclass",
            FileKind::Conf => "conf",
        }
    }
}

impl fmt::Display for FileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FileKind {
    type Err = Error;

    /// Inverse of [`FileKind::as_str`].
    fn from_str(s: &str) -> Result<FileKind, Error> {
        match s {
            "bb" => Ok(FileKind::Recipe),
            "bbappend" => Ok(FileKind::Append),
            "inc" => Ok(FileKind::Include),
            "bbclass" => Ok(FileKind::Class),
            "conf" => Ok(FileKind::Conf),
            _ => Err(Error::Invalid {
                msg: format!("unknown file kind {s:?}; use bb, bbappend, inc, bbclass or conf"),
            }),
        }
    }
}

/// Assignment operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum AssignOp {
    /// `=`
    Set,
    /// `?=`
    Default,
    /// `??=`
    WeakDefault,
    /// `:=`
    Immediate,
    /// `+=` (adds a space)
    Append,
    /// `=+` (adds a space)
    Prepend,
    /// `.=` (no space)
    AppendDot,
    /// `=.` (no space)
    PrependDot,
}

impl AssignOp {
    /// Every operator, longest text first (useful when matching source text).
    pub const ALL: [AssignOp; 8] = [
        AssignOp::WeakDefault,
        AssignOp::Default,
        AssignOp::Immediate,
        AssignOp::Append,
        AssignOp::Prepend,
        AssignOp::AppendDot,
        AssignOp::PrependDot,
        AssignOp::Set,
    ];

    /// The operator as written in source, e.g. `"?="`. Also the text stored in the index
    /// `statements.op` column.
    pub fn as_str(self) -> &'static str {
        match self {
            AssignOp::Set => "=",
            AssignOp::Default => "?=",
            AssignOp::WeakDefault => "??=",
            AssignOp::Immediate => ":=",
            AssignOp::Append => "+=",
            AssignOp::Prepend => "=+",
            AssignOp::AppendDot => ".=",
            AssignOp::PrependDot => "=.",
        }
    }
}

impl fmt::Display for AssignOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AssignOp {
    type Err = Error;

    /// Inverse of [`AssignOp::as_str`]; the text must be exactly an operator.
    fn from_str(s: &str) -> Result<AssignOp, Error> {
        AssignOp::ALL
            .into_iter()
            .find(|op| op.as_str() == s)
            .ok_or_else(|| Error::Invalid {
                msg: format!("unknown assignment operator {s:?}"),
            })
    }
}

/// The left-hand side of an assignment or a function name, split.
///
/// `"FILES:${PN}-dev:append"` -> base `"FILES"`, overrides `["${PN}-dev", "append"]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct VarRef {
    pub base: String,
    /// In source order, including `append` / `prepend` / `remove`.
    pub overrides: Vec<String>,
    /// `VAR[flag]`
    pub flag: Option<String>,
    /// Written with the legacy `_` override syntax.
    pub legacy_syntax: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(layer: &str, path: &str, a: u32, b: u32) -> Loc {
        Loc {
            layer: layer.into(),
            path: path.into(),
            lines: LineSpan::new(a, b),
        }
    }

    #[test]
    fn loc_round_trip_range() {
        let l = loc("meta-core", "recipes-core/hello/hello_1.0.bb", 3, 17);
        let s = l.to_string();
        assert_eq!(s, "meta-core:recipes-core/hello/hello_1.0.bb:3-17");
        assert_eq!(s.parse::<Loc>().unwrap(), l);
    }

    #[test]
    fn loc_round_trip_single_line() {
        let l = loc("meta-vendor", "conf/layer.conf", 5, 5);
        let s = l.to_string();
        assert_eq!(s, "meta-vendor:conf/layer.conf:5");
        assert_eq!(s.parse::<Loc>().unwrap(), l);
    }

    #[test]
    fn loc_round_trip_unusual_path() {
        let l = loc("meta-mine", "recipes-app/orphan/orphan_9.%.bbappend", 1, 2);
        assert_eq!(l.to_string().parse::<Loc>().unwrap(), l);
        let l = loc("build", "dir with spaces/a+b-c_1.0~rc1 (x)@#.bb", 10, 12);
        assert_eq!(l.to_string().parse::<Loc>().unwrap(), l);
        let l = loc("meta-ü", "files/ünïcode/ç.inc", 1, 1);
        assert_eq!(l.to_string().parse::<Loc>().unwrap(), l);
    }

    #[test]
    fn loc_parse_errors() {
        for bad in [
            "",
            "meta-core",
            "meta-core:path",
            ":path:1",
            "meta:",
            "meta::1",
            "meta:p:0",
            "meta:p:5-4",
            "meta:p:a-b",
            "meta:p:",
        ] {
            assert!(bad.parse::<Loc>().is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn file_kind_from_path() {
        let cases = [
            ("a/hello_1.0.bb", Some(FileKind::Recipe)),
            ("hello_%.bbappend", Some(FileKind::Append)),
            ("hello.inc", Some(FileKind::Include)),
            ("classes/base.bbclass", Some(FileKind::Class)),
            ("conf/layer.conf", Some(FileKind::Conf)),
            ("files/hello.patch", None),
            ("README", None),
            ("bb", None),
            ("x.BB", None),
        ];
        for (p, want) in cases {
            assert_eq!(FileKind::from_path(Path::new(p)), want, "{p}");
        }
    }

    #[test]
    fn file_kind_text_round_trip() {
        for k in [
            FileKind::Recipe,
            FileKind::Append,
            FileKind::Include,
            FileKind::Class,
            FileKind::Conf,
        ] {
            assert_eq!(k.as_str().parse::<FileKind>().unwrap(), k);
        }
        assert!("txt".parse::<FileKind>().is_err());
    }

    #[test]
    fn assign_op_text_mapping() {
        let cases = [
            (AssignOp::Set, "="),
            (AssignOp::Default, "?="),
            (AssignOp::WeakDefault, "??="),
            (AssignOp::Immediate, ":="),
            (AssignOp::Append, "+="),
            (AssignOp::Prepend, "=+"),
            (AssignOp::AppendDot, ".="),
            (AssignOp::PrependDot, "=."),
        ];
        for (op, text) in cases {
            assert_eq!(op.as_str(), text);
            assert_eq!(op.to_string(), text);
            assert_eq!(text.parse::<AssignOp>().unwrap(), op);
        }
        for bad in ["", "==", "-=", "? =", " ="] {
            assert!(bad.parse::<AssignOp>().is_err(), "{bad:?}");
        }
    }
}

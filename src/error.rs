//! Library error type. `anyhow` is used only in `main.rs`.

use std::path::PathBuf;

/// Every fallible library function returns this. Messages are one line and say what to do
/// next; they are shown to the MCP client as tool errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("index error: {0}; delete the index to rebuild it")]
    Sqlite(#[from] rusqlite::Error),

    #[error("configuration error: {msg}")]
    Config { msg: String },

    #[error("not found: {what}")]
    NotFound { what: String },

    #[error("ambiguous {what}; candidates: {}", candidates.join(", "))]
    Ambiguous {
        what: String,
        candidates: Vec<String>,
    },

    #[error("path {} is outside the workspace layers and build directory", path.display())]
    Confined { path: PathBuf },

    #[error("bitbake is not available: no initialised build environment was found")]
    BitbakeUnavailable,

    #[error("{msg}")]
    Invalid { msg: String },
}

impl Error {
    /// The error every M0 stub returns.
    pub fn not_implemented() -> Error {
        Error::Invalid {
            msg: "not implemented".into(),
        }
    }
}

impl From<toml::de::Error> for Error {
    fn from(e: toml::de::Error) -> Error {
        Error::Config { msg: e.to_string() }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_one_line() {
        let errs = [
            Error::not_implemented(),
            Error::NotFound {
                what: "recipe foo".into(),
            },
            Error::Ambiguous {
                what: "recipe hello".into(),
                candidates: vec!["hello_1.0".into(), "hello_1.1".into()],
            },
            Error::Confined {
                path: "/etc/passwd".into(),
            },
            Error::BitbakeUnavailable,
            Error::Config { msg: "x".into() },
        ];
        for e in errs {
            assert!(!e.to_string().contains('\n'), "{e}");
        }
        assert_eq!(Error::not_implemented().to_string(), "not implemented");
    }
}

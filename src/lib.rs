//! sous-chef: an MCP server for Yocto / BitBake layers. Module declarations only; see
//! `docs/architecture.md` for the layout and the contracts.

pub mod audit;
pub mod bitbake;
pub mod config;
pub mod error;
pub mod eval;
pub mod gitcmd;
pub mod handles;
pub mod hook;
pub mod index;
pub mod lint;
pub mod mcp;
pub mod parse;
pub mod release;
pub mod render;
pub mod scaffold;
pub mod types;
pub mod workspace;

pub use error::{Error, Result};

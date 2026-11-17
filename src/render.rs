//! All tool output goes through here. Contract: `docs/architecture.md` ("Rendering contract").
//!
//! Stub (M0): T2.4 implements `render`, `freshness_line`, the JSON form and truncation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::index::RefreshReport;

/// Default `budget_tokens` for every tool.
pub const DEFAULT_BUDGET_TOKENS: usize = 600;

/// Output format argument shared by every tool and the `lint` CLI.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// Tab-separated text, header row first.
    #[default]
    Text,
    /// The same data as JSON.
    Json,
}

/// A table: header plus rows of cells.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new(header: &[&str]) -> Table {
        Table {
            header: header.iter().map(|s| s.to_string()).collect(),
            rows: Vec::new(),
        }
    }

    pub fn row(&mut self, cells: &[&str]) {
        self.rows
            .push(cells.iter().map(|s| s.to_string()).collect());
    }

    /// Renders within the token budget, replacing rows that do not fit with a marker line.
    // Stub (M0): T2.4 implements it.
    pub fn render(&self, _budget_tokens: usize) -> String {
        let _ = (&self.header, &self.rows);
        String::new()
    }
}

/// Bytes / 4, rounded up.
pub fn estimate_tokens(s: &str) -> usize {
    s.len().div_ceil(4)
}

/// `reindexed N files: ...`, or `None` when nothing changed.
// Stub (M0): T2.4 implements it.
pub fn freshness_line(_r: &RefreshReport) -> Option<String> {
    None
}

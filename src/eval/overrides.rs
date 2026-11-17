//! Override finalisation: `OVERRIDES`, winning variant, `:append` / `:prepend` / `:remove`.
//!
//! Stub (M0): T3.3 implements these.

use super::{DataStore, Value};

impl DataStore {
    /// Finalised and expanded value of `name`.
    pub fn value(&self, _name: &str) -> Option<Value> {
        None
    }

    /// `OVERRIDES` entries, lowest priority first.
    pub fn overrides(&self) -> Vec<String> {
        Vec::new()
    }
}

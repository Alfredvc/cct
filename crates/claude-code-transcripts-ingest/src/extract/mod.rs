//! Structured JSON extraction from the ingested DuckDB.
//!
//! Each kind is its own submodule; this module just dispatches.
pub mod sessions;

use crate::cli::{ExtractArgs, ExtractKind};

pub fn run(args: ExtractArgs) {
    match args.kind {
        ExtractKind::Sessions(a) => sessions::run(a),
    }
}

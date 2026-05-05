//! Human-readable reports rendered from the ingested DuckDB.
//!
//! Each report kind is its own submodule; this module just dispatches.
pub mod usage;

use crate::cli::{ReportArgs, ReportKind};

pub fn run(args: ReportArgs) {
    match args.kind {
        ReportKind::Usage(a) => usage::run(a),
    }
}

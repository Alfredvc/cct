//! Library surface used by integration tests and re-used by main.rs.
//! The binary entry point still lives in main.rs.
pub mod cli;
pub mod cost_decomp;
pub mod extract;
pub mod info;
pub mod parse;
pub mod pricing;
pub mod report;
pub mod run;
pub mod schema;
pub mod scope;
pub mod serve;
pub mod update;
pub mod version_check;

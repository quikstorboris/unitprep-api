//! Shared, tool-agnostic engine for UnitPrep: session storage mechanics
//! and file ingestion/parsing. Every tool-specific crate (e.g.
//! `unitprep-unit-group`) depends on this; this crate must never depend
//! back on a tool-specific crate.

pub mod csv_document;
pub mod in_memory_session_store;
pub mod parsing;
pub mod session;
pub mod session_store;
pub mod uploaded_file;

#[cfg(test)]
mod parsing_tests;

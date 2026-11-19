//! Unofficial Rust library for Scalable Capital.
//!
//! Re-exports `scalable_cli::embed` from this fork of Scalable CLI under a name
//! that doesn't suggest the official CLI, and adds helpers for the types of
//! your own query results. See the repository README.

mod query_helpers;

pub use query_helpers::{Timeframe, decimal};
pub use scalable_cli::embed::*;

//! Agent-first audio workbench primitives. No model downloads or subprocesses.
#![forbid(unsafe_code)]

pub mod artifacts;
pub mod audio;
pub mod control;
pub mod error;
pub mod journal;
pub mod runtime;
pub mod transcript;
pub mod types;

pub use error::{Error, Result};

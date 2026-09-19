//! CVLR specification for Redis -- user-visible behavior, including concurrency.
//!
//! See PROPERTIES.md for the numbered property catalog, FINDINGS.md for open leads, and
//! ../CLAUDE.md for the toolchain and the traps.
//!
//! NOTE: this crate is deliberately `std`, not `no_std`. cvlr's `#[rule]` inserts
//! `cvlr_rule_location!()`, which expands to `std::file!()` (cvlr-log/src/core.rs:243-248)
//! and will not compile under `#![no_std]`.

pub mod driver;
pub mod model;

#[cfg(feature = "certora")]
pub mod specs;

//! CVLR rules.
//!
//! Naming: rule names are GLOBAL symbols in the compiled wasm, so every rule carries an
//! area prefix -- `expire_`, `string_`, `keyspace_`, `multi_`.
//!
//! Categories follow the stellar-contracts taxonomy (see ../CLAUDE.md):
//!   _integrity   post-state related to pre-state for one operation
//!   _invariants  one rule per (invariant x mutating operation)
//!   _interleaving  NEW: bounded schedules, for client-visible concurrency
//!
//! Nothing here has been run through the Prover -- there is no Certora tooling on this
//! machine. Every `status:` below is `unproven` until that changes. Never write
//! `status: verified` on the strength of a compile.

pub mod expire_rules;
pub mod keyspace_rules;
pub mod multi_rules;
pub mod string_rules;

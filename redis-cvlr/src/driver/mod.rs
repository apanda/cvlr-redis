//! Concrete execution of the SAME `#[rule]` bodies the Prover checks, plus differential
//! testing of the model against a real redis-server.
//!
//! WHY THIS EXISTS. There is no Certora tooling on this machine and access is not on a
//! known timeline (see ../CLAUDE.md). A specification whose only evidence is a Prover run
//! nobody can do is not evidence. CVLR's entire vocabulary is `extern "C"` CVT_* symbols
//! the Prover *interprets*; nothing stops us from *implementing* them. Then the identical
//! rule bodies execute over a byte stream, and the schedules they draw can be replayed
//! against a real redis-server.
//!
//! This is what makes "draw, do not filter" a hard style rule rather than a preference:
//! `nondet::<u64>() % K` yields a usable value on every run, while `cvlr_assume!(k < K)`
//! rejects essentially all of them.
//!
//! The differential leg is the ONLY evidence that touches the shipped C. It is sampling,
//! not proof, and it does not shrink the correspondence gap asymptotically -- but it is
//! the difference between a specification and a fiction.

#[cfg(feature = "rt")]
pub mod concrete;

#[cfg(feature = "rt")]
pub mod difftest;

#[cfg(feature = "rt")]
pub mod resp;

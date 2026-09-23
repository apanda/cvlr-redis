//! MILESTONE 0. Not properties -- a check that this leg can report BOTH outcomes.
//!
//! The CVLR leg skipped this step and it cost a day: a setup that reports PASS on
//! everything is silent, not correct. `smoke_expect_fail` below MUST fail verification.
//! It is behind a feature so the crate verifies clean by default; run
//! `just verify-negative` to confirm it still fails.

use vstd::prelude::*;
use crate::model::state::*;

verus! {

/// Expected: VERIFIES. Exercises Seq indexing, the World struct and a real implication.
pub proof fn smoke_expect_verify(w: World, k: int)
    requires
        w.valid_key(k),
        w.standalone_master(),
        w.slots[k].present,
    ensures
        w.slots.len() > 0,
{
}

/// Expected: FAILS. If this ever verifies, the setup is reporting nothing.
#[cfg(feature = "negative")]
pub proof fn smoke_expect_fail(w: World, k: int)
    requires w.valid_key(k),
    ensures w.slots[k].present,      // FALSE: nothing constrains this
{
}

} // verus!

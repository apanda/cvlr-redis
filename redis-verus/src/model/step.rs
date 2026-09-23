//! The transition relation, and the active expire cycle as a real loop.
//!
//! This is where this leg diverges from ../../redis-cvlr. CVLR can check that ONE drawn
//! command preserves an invariant, at K = 3, by bounded unrolling. It cannot state "holds
//! in every state reachable by any sequence of commands, for any keyspace size" -- that is
//! induction over the transition relation, which BMC has no way to express.

use vstd::prelude::*;
use crate::model::state::*;
use crate::model::expire::*;
use crate::model::cmd::*;

verus! {

/// One step of the system. `ActiveExpire` is an ADVERSARIAL step, not a background detail
/// to be assumed away: it may be interleaved anywhere, which turns "a key may vanish at
/// any moment" from prose into a hypothesis every other property must survive.
#[derive(PartialEq, Eq, Structural, Clone, Copy)]
pub enum Step {
    Get { key: usize },
    ActiveExpire,
    ClockTick { delta: i64 },
}

/// The active expire cycle -- expire.c:40-41. A LOOP over the whole keyspace, of any
/// size, discharged by an invariant rather than by unrolling K = 3 times.
///
/// On a replica the cycle does not delete: expiry there is master-driven.
pub fn active_expire_cycle(w: &mut World)
    requires wf(*old(w)),
    ensures
        wf(*final(w)),
        same_config(*final(w), *old(w)),
        final(w).slots@.len() == old(w).slots@.len(),
        // P-11, for ANY keyspace size:
        // (a) never creates a key
        forall|j: int| #![auto] 0 <= j < final(w).slots@.len()
            ==> (final(w).slots@[j].present ==> old(w).slots@[j].present),
        // (b) survivors are untouched
        forall|j: int| #![auto] 0 <= j < final(w).slots@.len()
            ==> (final(w).slots@[j].present ==> final(w).slots@[j] == old(w).slots@[j]),
        // (c) anything removed was genuinely due
        forall|j: int| #![auto] 0 <= j < final(w).slots@.len()
            ==> (old(w).slots@[j].present && !final(w).slots@[j].present
                 ==> active_cycle_would_expire(*old(w), j)),
{
    if w.role == Role::ReadOnlyReplica || w.expire_paused {
        return;
    }
    let ghost old_w = *w;
    let mut i: usize = 0;
    while i < w.slots.len()
        invariant
            i <= w.slots@.len(),
            w.slots@.len() == old_w.slots@.len(),
            w.clock == old_w.clock,
            w.role == old_w.role,
            w.expire_paused == old_w.expire_paused,
            w.loading == old_w.loading,
            w.allow_access_expired == old_w.allow_access_expired,
            wf(*w),
            // untouched suffix
            forall|j: int| #![auto] i <= j < w.slots@.len() ==> w.slots@[j] == old_w.slots@[j],
            // processed prefix satisfies (a), (b), (c)
            forall|j: int| #![auto] 0 <= j < i
                ==> (w.slots@[j].present ==> old_w.slots@[j].present),
            forall|j: int| #![auto] 0 <= j < i
                ==> (w.slots@[j].present ==> w.slots@[j] == old_w.slots@[j]),
            forall|j: int| #![auto] 0 <= j < i
                ==> (old_w.slots@[j].present && !w.slots@[j].present
                     ==> active_cycle_would_expire(old_w, j)),
        decreases w.slots@.len() - i,
    {
        let due = w.slots[i].present
            && w.slots[i].expire_at >= 0
            && w.clock >= w.slots[i].expire_at;
        if due {
            w.slots.set(i, Slot { present: false, expire_at: NO_EXPIRE });
            w.repl.push(Effect::Del { key: i });
        }
        i = i + 1;
    }
}

/// A step is applicable to `w` when its key is in range and its clock delta is sane.
/// Factored out of the quantifiers below: a bare `match` in a quantifier body gives Verus
/// no term to trigger on.
pub open spec fn step_ok(w: World, s: Step) -> bool {
    match s {
        Step::Get { key } => key < w.slots@.len(),
        Step::ClockTick { delta } => 0 <= delta && delta <= 1000,
        Step::ActiveExpire => true,
    }
}

/// Apply one step.
pub fn step(w: &mut World, s: Step)
    requires
        wf(*old(w)),
        step_ok(*old(w), s),
        old(w).clock <= 0x3fff_ffff_ffff_ffff - 1000,
    ensures
        wf(*final(w)),
        final(w).slots@.len() == old(w).slots@.len(),
        // the whole config frame is untouched except the clock, which only ClockTick moves
        final(w).role == old(w).role,
        final(w).caller == old(w).caller,
        final(w).loading == old(w).loading,
        final(w).allow_access_expired == old(w).allow_access_expired,
        final(w).expire_paused == old(w).expire_paused,
        final(w).cluster_enabled == old(w).cluster_enabled,
        final(w).conf_allows_expire_del == old(w).conf_allows_expire_del,
        // the clock only ever moves forward, and only ClockTick moves it
        old(w).clock <= final(w).clock <= old(w).clock + 1000,
        // NO RESURRECTION: no step in this alphabet ever makes an absent key present.
        forall|j: int| #![auto] 0 <= j < final(w).slots@.len()
            ==> (final(w).slots@[j].present ==> old(w).slots@[j].present),
{
    match s {
        Step::Get { key } => { let _ = cmd_get(w, key); }
        Step::ActiveExpire => { active_expire_cycle(w); }
        Step::ClockTick { delta } => { w.clock = w.clock + delta; }
    }
}

/// THE INDUCTIVE PROPERTY, and the one CVLR structurally cannot state.
///
/// Over an arbitrary-length sequence of steps, on a keyspace of arbitrary size:
///
///   1. well-formedness (P-05) is preserved -- not "one drawn command preserves it at
///      K = 3", but "every state reachable by any sequence satisfies it";
///   2. NO RESURRECTION -- the set of physically present keys is monotonically
///      non-increasing. No read, clock tick or expire cycle can bring a deleted key back.
///
/// CVLR's P-05 assumes the invariant, runs ONE drawn command, and re-asserts it, with
/// K = 3 and `optimistic_loop` assuming away anything longer. This is the general theorem.
pub fn run(w: &mut World, steps: &Vec<Step>)
    requires
        wf(*old(w)),
        forall|i: int| 0 <= i < steps@.len() ==> step_ok(*old(w), #[trigger] steps@[i]),
        old(w).clock <= 0x0fff_ffff_ffff_ffff,
        steps@.len() <= 1_000_000,
    ensures
        wf(*final(w)),
        final(w).slots@.len() == old(w).slots@.len(),
        forall|j: int| #![auto] 0 <= j < final(w).slots@.len()
            ==> (final(w).slots@[j].present ==> old(w).slots@[j].present),
{
    let ghost w0 = *w;
    let mut i: usize = 0;
    while i < steps.len()
        invariant
            i <= steps@.len(),
            wf(*w),
            w.slots@.len() == w0.slots@.len(),
            w.clock <= 0x0fff_ffff_ffff_ffff + (i as int) * 1000,
            steps@.len() <= 1_000_000,
            forall|j: int| #![auto] 0 <= j < w.slots@.len()
                ==> (w.slots@[j].present ==> w0.slots@[j].present),
            forall|m: int| 0 <= m < steps@.len() ==> step_ok(w0, #[trigger] steps@[m]),
        decreases steps@.len() - i,
    {
        step(w, steps[i]);
        i = i + 1;
    }
}

} // verus!

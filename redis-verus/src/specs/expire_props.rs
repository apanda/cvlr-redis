//! P-10 and P-10b, stated to match ../../redis-cvlr/src/specs/expire_rules.rs
//! assertion-for-assertion. These are the two smallest of the six properties Certora
//! reports Violated.
//!
//! Each runs the SAME command the CVLR rule runs (`cmd_get`) and asserts about the POST
//! state, including the replication log -- not a spec predicate over the pre-state. The
//! only intended difference from the CVLR leg is generality: `k` is any valid index into
//! a keyspace of any length, not one of `K = 3`.

use vstd::prelude::*;
use crate::model::state::*;
use crate::model::expire::*;
use crate::model::cmd::*;

verus! {

/// P-10. Replica-Hides-But-Does-Not-Delete.
///
/// On a read-only replica a logically expired key is invisible to a normal client but
/// remains PHYSICALLY present -- expiry there is driven by DELs from the master
/// (db.c:3042-3045). A genuine master/replica observable divergence: DBSIZE disagrees
/// between them for the same logical dataset.
///
/// CVLR: `expire_replica_hides_without_deleting`
///   cvlr_assert!(g == Reply::Nil)          -> r == Reply::Nil
///   cvlr_assert!(w.slots[k].present)       -> final(w).slots@[k].present
///   cvlr_assert!(w.repl.len == before)     -> final(w).repl@ == old(w).repl@
pub fn p10_replica_hides_without_deleting(w: &mut World, k: usize) -> (r: Reply)
    requires
        old(w).valid_key(k as int),
        wf(*old(w)),
        old(w).role == Role::ReadOnlyReplica,
        old(w).caller == Caller::Normal,
        !old(w).loading,
        !old(w).allow_access_expired,
        !old(w).expire_paused,
        !old(w).cluster_enabled,
        old(w).conf_allows_expire_del,
        old(w).slots@[k as int].present,
        key_is_expired(*old(w), k as int),
    ensures
        r == Reply::Nil,                                  // invisible
        final(w).slots@[k as int].present,                // but still there
        final(w).repl@ == old(w).repl@,                   // replica propagated nothing
{
    cmd_get(w, k)
}

/// P-10b. Master-Link-Client-Never-Sees-Expiry.
///
/// A command arriving on the replication link (`CLIENT_MASTER`) treats a key past its TTL
/// as VALID (db.c:3043). Expiry is a function of *who is asking*, not only of the state.
///
/// CVLR: `expire_master_link_sees_expired_key_as_valid`
///   cvlr_assert!(g != Reply::Nil)          -> r != Reply::Nil
///   cvlr_assert!(w.slots[k].present)       -> final(w).slots@[k].present
pub fn p10b_master_link_sees_expired_key_as_valid(w: &mut World, k: usize) -> (r: Reply)
    requires
        old(w).valid_key(k as int),
        wf(*old(w)),
        old(w).role == Role::ReadOnlyReplica,
        old(w).caller == Caller::MasterLink,
        !old(w).loading,
        !old(w).allow_access_expired,
        !old(w).expire_paused,
        !old(w).cluster_enabled,
        old(w).conf_allows_expire_del,
        old(w).slots@[k as int].present,
        key_is_expired(*old(w), k as int),
    ensures
        r != Reply::Nil,
        final(w).slots@[k as int].present,
        final(w).repl@ == old(w).repl@,
{
    cmd_get(w, k)
}

/// The two are genuine opposites on the same state: the ONLY difference is `caller`.
/// A spec error that made both vacuous would fail here.
pub proof fn p10_and_p10b_are_opposites(w: World, k: int)
    requires
        w.valid_key(k),
        w.role == Role::ReadOnlyReplica,
        !w.loading, !w.allow_access_expired, !w.expire_paused, !w.cluster_enabled,
        w.conf_allows_expire_del,
        w.slots@[k].present,
        key_is_expired(w, k),
    ensures
        (expire_status(w, k, false) == KeyStatus::Valid) <==> (w.caller == Caller::MasterLink),
{
}

/// NON-VACUITY, executable. Verus has no `rule_sanity` equivalent: an `exec fn` whose
/// `requires` are unsatisfiable verifies trivially and establishes nothing. These build a
/// concrete `World` that meets each precondition and actually CALL the property, so
/// "verified" above cannot mean "vacuous". They also double as the smallest end-to-end
/// exercise of the executable model.
pub fn p10_witness() -> (r: Reply)
    ensures r == Reply::Nil,
{
    let mut slots: Vec<Slot> = Vec::new();
    slots.push(Slot { present: true, expire_at: 5 });
    let mut w = World {
        slots,
        clock: 10,                       // 10 > 5, so the key is logically expired
        role: Role::ReadOnlyReplica,
        caller: Caller::Normal,
        loading: false,
        allow_access_expired: false,
        expire_paused: false,
        cluster_enabled: false,
        conf_allows_expire_del: true,
        repl: Vec::new(),
    };
    assert(w.slots@.len() == 1);
    assert(w.valid_key(0int));
    assert(key_is_expired(w, 0int));
    p10_replica_hides_without_deleting(&mut w, 0)
}

pub fn p10b_witness() -> (r: Reply)
    ensures r != Reply::Nil,
{
    let mut slots: Vec<Slot> = Vec::new();
    slots.push(Slot { present: true, expire_at: 5 });
    let mut w = World {
        slots,
        clock: 10,
        role: Role::ReadOnlyReplica,
        caller: Caller::MasterLink,      // the only difference from p10_witness
        loading: false,
        allow_access_expired: false,
        expire_paused: false,
        cluster_enabled: false,
        conf_allows_expire_del: true,
        repl: Vec::new(),
    };
    assert(w.slots@.len() == 1);
    assert(w.valid_key(0int));
    assert(key_is_expired(w, 0int));
    p10b_master_link_sees_expired_key_as_valid(&mut w, 0)
}

} // verus!

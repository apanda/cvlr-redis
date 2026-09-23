//! Commands -- the EXECUTABLE model. `exec fn`s taking `&mut World`, so properties are
//! stated about the POST state exactly as the CVLR leg's rules are, and so the model can
//! be compiled and driven by the differential harness.

use vstd::prelude::*;
use crate::model::state::*;
use crate::model::expire::*;

verus! {

#[derive(PartialEq, Eq, Structural, Clone, Copy)]
pub enum Reply {
    Nil,
    Int(i64),
    /// A value was returned. Byte content is not modelled at this stage.
    Present,
}

/// `deleteExpiredKeyAndPropagate` -> `deleteKeyAndPropagate` (db.c:2898-2900).
/// The propagated DEL is a standalone op, deliberately NOT wrapped in MULTI/EXEC --
/// pinned by redis/tests/unit/expire.tcl:809 and :830.
pub fn delete_expired_key_and_propagate(w: &mut World, k: usize)
    requires old(w).valid_key(k as int), wf(*old(w)),
    ensures
        wf(*final(w)),
        same_config(*final(w), *old(w)),
        final(w).slots@.len() == old(w).slots@.len(),
        final(w).slots@[k as int] == (Slot { present: false, expire_at: NO_EXPIRE }),
        forall|j: int| #![auto] 0 <= j < final(w).slots@.len() && j != k as int
            ==> final(w).slots@[j] == old(w).slots@[j],
        final(w).repl@ == old(w).repl@.push(Effect::Del { key: k }),
{
    w.slots.set(k, Slot { present: false, expire_at: NO_EXPIRE });
    w.repl.push(Effect::Del { key: k });
}

/// `lookupKeyRead` -- the read path. Returns whether the value is visible to the caller,
/// and performs the lazy delete when `expireIfNeeded` returns KEY_DELETED.
pub fn lookup_key_read(w: &mut World, k: usize) -> (visible: bool)
    requires old(w).valid_key(k as int), wf(*old(w)),
    ensures
        wf(*final(w)),
        same_config(*final(w), *old(w)),
        forall|j: int| #![auto] 0 <= j < final(w).slots@.len()
            ==> (final(w).slots@[j].present ==> old(w).slots@[j].present),
        final(w).slots@.len() == old(w).slots@.len(),
        visible == (old(w).slots@[k as int].present
                    && expire_status(*old(w), k as int, false) == KeyStatus::Valid),
        final(w).slots@[k as int].present
            == (old(w).slots@[k as int].present
                && !(expire_status(*old(w), k as int, false) == KeyStatus::Deleted)),
        final(w).repl@ == if old(w).slots@[k as int].present
                             && expire_status(*old(w), k as int, false) == KeyStatus::Deleted {
            old(w).repl@.push(Effect::Del { key: k })
        } else {
            old(w).repl@
        },
{
    if !w.slots[k].present {
        return false;
    }
    let s = expire_status_exec(w, k, false);
    if s == KeyStatus::Deleted {
        delete_expired_key_and_propagate(w, k);
        false
    } else {
        s == KeyStatus::Valid
    }
}

/// `GET`.
pub fn cmd_get(w: &mut World, k: usize) -> (r: Reply)
    requires old(w).valid_key(k as int), wf(*old(w)),
    ensures
        wf(*final(w)),
        same_config(*final(w), *old(w)),
        forall|j: int| #![auto] 0 <= j < final(w).slots@.len()
            ==> (final(w).slots@[j].present ==> old(w).slots@[j].present),
        final(w).slots@.len() == old(w).slots@.len(),
        r == if old(w).slots@[k as int].present
                && expire_status(*old(w), k as int, false) == KeyStatus::Valid {
            Reply::Present
        } else {
            Reply::Nil
        },
        final(w).slots@[k as int].present
            == (old(w).slots@[k as int].present
                && !(expire_status(*old(w), k as int, false) == KeyStatus::Deleted)),
        final(w).repl@ == if old(w).slots@[k as int].present
                             && expire_status(*old(w), k as int, false) == KeyStatus::Deleted {
            old(w).repl@.push(Effect::Del { key: k })
        } else {
            old(w).repl@
        },
{
    if lookup_key_read(w, k) { Reply::Present } else { Reply::Nil }
}

} // verus!

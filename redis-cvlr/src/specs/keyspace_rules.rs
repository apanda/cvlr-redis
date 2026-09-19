//! property: P-05. Expires-Subset-Of-Keys.
//! property: P-11. Active-Expiry-Only-Removes.

use cvlr::prelude::*;

use crate::model::{cmd::*, expire::*, state::*, step::*};

/// property: P-05. Expires-Subset-Of-Keys.
/// description: no absent slot carries a TTL. In Redis 8.9.241 this is STRUCTURAL rather
///   than an invariant to prove -- the value is a `kvobj` embedding both the key and the
///   expire, and db->keys / db->expires are kvstores over the SAME pointers
///   (object.h:31-66). The rule exists to pin that the MODEL preserves it under every
///   operation, which is what would break first if someone refactors the state.
/// status: unproven
#[rule]
pub fn keyspace_expires_subset_of_keys() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();

    // Precondition: holds in the pre-state (it is structural in nondet_world).
    let mut i = 0;
    while i < K {
        cvlr_assume!(w.slots[i].present || !w.slots[i].has_ttl());
        i += 1;
    }

    let c = draw_client();
    step(&mut w, Step::Cmd(c, draw_cmd(c)));

    let mut j = 0;
    while j < K {
        cvlr_assert!(w.slots[j].present || !w.slots[j].has_ttl());
        j += 1;
    }
}

/// property: P-11. Active-Expiry-Only-Removes.
/// description: the active expire cycle never creates a key, never changes a surviving
///   key's value, and only removes keys whose TTL has elapsed. It is an ADVERSARIAL
///   environment step, not a postcondition of any command.
/// evidence: expire.c:40-41.
/// status: unproven
#[rule]
pub fn keyspace_active_expiry_only_removes() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();
    let pre = w;

    active_expire_cycle(&mut w);

    let mut i = 0;
    while i < K {
        // Never creates.
        cvlr_assert!(!(w.slots[i].present && !pre.slots[i].present));
        // Survivors are untouched...
        if w.slots[i].present {
            cvlr_assert!(w.slots[i].value == pre.slots[i].value);
            cvlr_assert!(w.slots[i].expire_at == pre.slots[i].expire_at);
        }
        // ...and anything removed was genuinely due.
        if pre.slots[i].present && !w.slots[i].present {
            cvlr_assert!(pre.slots[i].has_ttl());
            cvlr_assert!(pre.slots[i].expire_at <= pre.clock);
        }
        i += 1;
    }
}

/// property: P-12. DBSIZE-Counts-Physical-Presence.
/// description: DBSIZE equals the number of physically present slots, with no expiry
///   filtering whatsoever, and DBSIZE itself never mutates the keyspace.
/// evidence: db.c:3148-3173.
/// status: unproven
#[rule]
pub fn keyspace_dbsize_counts_physical() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();
    let pre = w;

    let r = dispatch(&mut w, 0, Cmd::DbSize);

    let mut expected = 0i64;
    let mut i = 0;
    while i < K {
        if pre.slots[i].present {
            expected += 1;
        }
        i += 1;
    }
    cvlr_assert!(r == Reply::Int(expected));

    // DBSIZE is a pure observation.
    let mut j = 0;
    while j < K {
        cvlr_assert!(w.slots[j].present == pre.slots[j].present);
        j += 1;
    }
    cvlr_assert!(w.repl.len == pre.repl.len);
}

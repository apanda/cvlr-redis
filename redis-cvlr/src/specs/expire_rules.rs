//! property: P-02. Expiry-Visibility-Split.      <- the flagship correction to folklore
//! property: P-03. Lazy-vs-Active-Off-By-One.
//! property: P-04. Expire-Condition-Semantics.
//! property: P-09. Lazy-Expiry-Propagates-A-Bare-DEL.
//! property: P-10. Replica-Hides-But-Does-Not-Delete.

use cvlr::prelude::*;

use crate::model::{cmd::*, expire::*, state::*, step::*};

/// property: P-02. Expiry-Visibility-Split.
/// description: on a standalone master, for a key whose TTL has elapsed but which has not
///   yet been touched: the lookup path reports it absent, `KEYS` hides it WITHOUT
///   deleting, and `DBSIZE` still counts it. The folklore statement "a key past its TTL is
///   never observed" is FALSE; only the lookup-path version is true.
/// evidence: db.c:3148-3173 (dbSize, no filter), db.c:1638-1645 (KEYS filters, no delete),
///   db.c:3004-3073 (expireIfNeeded deletes on the lookup path).
/// oracle: measured on redis-server 8.9.241 -- DBSIZE 1, KEYS [] , DBSIZE still 1,
///   EXISTS 0, DBSIZE 0.
/// status: unproven
#[rule]
pub fn expire_visibility_split() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();
    let k = draw_key();

    // Constrain to the interesting case by CONSTRUCTION, not by assumption.
    w.slots[k].present = true;
    w.slots[k].expire_at = w.clock - 1 - (nondet_range(1000) as Ms); // strictly in the past
    cvlr_assert!(key_is_expired(&w, k));

    // KEYS: hides it, does not delete it.
    let before = w;
    let keys = dispatch(&mut w, 0, Cmd::Keys);
    if let Reply::KeySet(set) = keys {
        cvlr_assert!(!set[k]);
    }
    cvlr_assert!(w.slots[k].present); // <-- KEYS did NOT delete
    cvlr_assert!(w.repl.len == before.repl.len); // and propagated nothing

    // DBSIZE: still counts it, because it is physically present.
    let n_before = match dispatch(&mut w, 0, Cmd::DbSize) {
        Reply::Int(n) => n,
        _ => -1,
    };
    cvlr_assert!(w.slots[k].present);

    // EXISTS: goes through lookupKeyRead, so it reports absent AND deletes.
    let e = dispatch(&mut w, 0, Cmd::Exists { key: k });
    cvlr_assert!(e == Reply::Int(0));
    cvlr_assert!(!w.slots[k].present); // <-- EXISTS DID delete

    let n_after = match dispatch(&mut w, 0, Cmd::DbSize) {
        Reply::Int(n) => n,
        _ => -1,
    };
    clog!(n_before);
    clog!(n_after);
    cvlr_assert!(n_after == n_before - 1);
}

/// property: P-03. Lazy-vs-Active-Off-By-One.
/// description: at exactly `clock == expire_at`, the lazy lookup path considers the key
///   VALID while the active expire cycle deletes it. The comparisons differ by one
///   millisecond and that difference is client-visible.
/// evidence: db.c:2954 `return now > when;` (strict) vs expire.c:40-41
///   `if (now < expire) return 0;` (i.e. expires at now >= when).
/// status: unproven
#[rule]
pub fn expire_lazy_active_boundary() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();
    let k = draw_key();

    w.slots[k].present = true;
    w.slots[k].expire_at = w.clock; // exactly at the boundary

    // Lazy: NOT expired (strict >).
    cvlr_assert!(!key_is_expired(&w, k));
    // Active: WOULD expire (>=).
    cvlr_assert!(active_cycle_would_expire(&w, k));

    // So a read at this instant still returns the value...
    let g = dispatch(&mut w, 0, Cmd::Get { key: k });
    cvlr_assert!(g != Reply::Nil);
    cvlr_assert!(w.slots[k].present);

    // ...while the active cycle at the same clock removes it.
    active_expire_cycle(&mut w);
    cvlr_assert!(!w.slots[k].present);
}

/// property: P-04. Expire-Condition-Semantics.
/// description: EXPIRE NX/XX/GT/LT return 0 without changing the TTL exactly when their
///   condition fails, and a key with no TTL counts as +infinity -- so GT always fails on
///   it and LT always succeeds.
/// evidence: expire.c:772 (NX), :780 (XX), :789 (GT), :800 (LT).
/// status: unproven
#[rule]
pub fn expire_condition_semantics() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();
    let k = draw_key();

    // Make the key logically live so the command gets past lookupKeyWrite.
    w.slots[k].present = true;
    w.slots[k].expire_at = if draw_bool() { NO_EXPIRE } else { w.clock + 1 + nondet_range(1000) as Ms };

    let current = w.slots[k].expire_at;
    let at = w.clock + 1 + nondet_range(2000) as Ms; // strictly in the future
    let cond = draw_expire_cond();

    let r = dispatch(&mut w, 0, Cmd::Expire { key: k, at, cond });
    let applied = r == Reply::Int(1);

    let expected = match cond {
        ExpireCond::None => true,
        ExpireCond::Nx => current == NO_EXPIRE,
        ExpireCond::Xx => current != NO_EXPIRE,
        // A persistent key is infinite: GT can never beat it.
        ExpireCond::Gt => current != NO_EXPIRE && at > current,
        // A persistent key is infinite: LT always beats it.
        ExpireCond::Lt => current == NO_EXPIRE || at < current,
    };
    clog!(at);
    cvlr_assert!(applied == expected);

    if applied {
        cvlr_assert!(w.slots[k].expire_at == at);
    } else {
        cvlr_assert!(w.slots[k].expire_at == current);
    }
}

/// property: P-09. Lazy-Expiry-Propagates-A-Bare-DEL.
/// description: a read that lazily expires a key propagates exactly one DEL, and that DEL
///   is NOT wrapped in MULTI/EXEC.
/// evidence: db.c:2898 deleteExpiredKeyAndPropagate. The DEL is bare not because expiry is
///   special-cased, but because such a unit emits exactly ONE op, and one-op units are not
///   framed (server.c:4005). Confirmed against the real server: a READ that lazily expires
///   propagates `DEL`, while a WRITE onto an expired key emits two ops and propagates
///   `MULTI DEL SET EXEC` -- see `lazy_expire_framing_matches_real_redis`.
/// oracle: tests/unit/expire.tcl:809 and :830 pin the bare, unwrapped DEL.
/// status: unproven
#[rule]
pub fn expire_lazy_propagates_bare_del() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();
    let k = draw_key();

    w.slots[k].present = true;
    w.slots[k].expire_at = w.clock - 1 - nondet_range(1000) as Ms;
    let before = w.repl.len;

    dispatch(&mut w, 0, Cmd::Get { key: k });

    cvlr_assert!(w.repl.len == before + 1);
    cvlr_assert!(w.repl.get(before) == Some(Effect::Del { key: k }));
    // Not framed: a single op is never wrapped (server.c:4005).
    cvlr_assert!(w.repl.get(before) != Some(Effect::Multi));
}

/// property: P-10. Replica-Hides-But-Does-Not-Delete.
/// description: on a read-only replica a logically expired key is invisible to a normal
///   client but remains PHYSICALLY present -- expiry there is driven by DELs from the
///   master. This is a genuine master/replica observable divergence: DBSIZE disagrees.
/// evidence: db.c:3042-3045.
/// status: unproven
#[rule]
pub fn expire_replica_hides_without_deleting() {
    let mut w = World::nondet_world();
    w.role = Role::ReadOnlyReplica;
    w.caller = Caller::Normal;
    w.allow_access_expired = false;
    w.expire_paused = false;
    w.cluster_enabled = false;
    w.conf_allows_expire_del = true;

    let k = draw_key();
    w.slots[k].present = true;
    w.slots[k].expire_at = w.clock - 1 - nondet_range(1000) as Ms;
    let before = w.repl.len;

    let g = dispatch(&mut w, 0, Cmd::Get { key: k });

    cvlr_assert!(g == Reply::Nil);       // invisible
    cvlr_assert!(w.slots[k].present);     // but still there
    cvlr_assert!(w.repl.len == before);   // and the replica propagated nothing
}

/// property: P-10b. Master-Link-Client-Never-Sees-Expiry.
/// description: a command arriving on the replication link (CLIENT_MASTER) treats a key
///   past its TTL as VALID.
/// evidence: db.c:3043.
/// status: unproven
#[rule]
pub fn expire_master_link_sees_expired_key_as_valid() {
    let mut w = World::nondet_world();
    w.role = Role::ReadOnlyReplica;
    w.caller = Caller::MasterLink;
    w.allow_access_expired = false;
    w.expire_paused = false;
    w.cluster_enabled = false;
    w.conf_allows_expire_del = true;

    let k = draw_key();
    w.slots[k].present = true;
    w.slots[k].value = Value::Str(draw_str());
    w.slots[k].expire_at = w.clock - 1 - nondet_range(1000) as Ms;

    let g = dispatch(&mut w, 0, Cmd::Get { key: k });
    cvlr_assert!(g != Reply::Nil);
    cvlr_assert!(w.slots[k].present);
}

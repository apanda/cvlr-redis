//! property: P-07. WATCH-Is-Value-Blind.
//! property: P-08. Touch-Dirties-Every-Watcher.   <- EXPECTED TO FAIL; see ../../FINDINGS.md F-01
//!
//! These are `_interleaving` rules: CVLR has no concurrency vocabulary at all, so the
//! schedule is reified into `World` (a per-key ORDERED watcher list, plus explicit steps
//! by distinct clients) and quantified over with nondet.

use cvlr::prelude::*;

use crate::model::{cmd::*, state::*, step::*, watch::*};

/// property: P-07. WATCH-Is-Value-Blind.
/// description: a write to a watched key dirties the watcher's CAS even when the write
///   restores the key's original value. CAS is key-name-based, not value-based.
/// evidence: multi.c:387-425 -- touchWatchedKey never inspects the value.
/// note: the `cvlr_assume!` that the value is unchanged is the POINT of this rule. It
///   makes the property state something a value-comparison CAS model would get wrong.
/// status: unproven
#[rule]
pub fn multi_watch_is_value_blind() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();

    let k = draw_key();
    // A live key with a known value, so the rewrite genuinely restores it.
    let v = draw_str();
    w.slots[k].present = true;
    w.slots[k].value = Value::Str(v);
    w.slots[k].expire_at = NO_EXPIRE;

    let watcher: ClientId = 0;
    let writer: ClientId = 1;
    watch_key(&mut w, watcher, k);
    cvlr_assert!(!w.clients[watcher].dirty_cas);

    // Write the SAME value back.
    exec_cmd(&mut w, writer, Cmd::Set { key: k, val: v, cond: SetCond::Always, ttl: TtlArg::None, get: false });

    // Value is unchanged...
    cvlr_assert!(w.slots[k].value == Value::Str(v));
    // ...and the watcher is dirty anyway.
    cvlr_assert!(w.clients[watcher].dirty_cas);
}

/// property: P-08. Touch-Dirties-Every-Watcher.
/// description: when a watched key is modified, EVERY client watching it that did not see
///   it already-logically-expired at WATCH time has its CAS dirtied -- regardless of that
///   client's position in the per-key watcher list.
/// evidence: multi.c:405-416. The `break` at multi.c:415 abandons the whole list.
/// status: EXPECTED TO FAIL against the model.
///
/// This rule deliberately targets FINDINGS.md F-01. The model transcribes the `break` as
/// written, so if the anomaly has teeth this rule produces a counterexample naming the
/// skipped watcher.
///
/// A FAILURE HERE IS NOT A REDIS BUG. It localizes a discrepancy that `drivers/difftest`
/// must then confirm against a real redis-server. Note that a direct client-visible repro
/// was ALREADY ATTEMPTED and failed -- lazy expiry clears `wk->expired` before the key
/// becomes present again -- so the expected outcome is that this rule fails on the model
/// while the real server behaves correctly, which would mean the MODEL is too literal and
/// is missing the flag-clearing pass. Either way the discrepancy is worth surfacing.
#[rule]
pub fn multi_touch_dirties_every_watcher() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();

    let k = draw_key();
    w.slots[k].present = true;
    w.slots[k].value = Value::Str(draw_str());
    // Logically expired, so the FIRST watcher records wk->expired = 1.
    w.slots[k].expire_at = w.clock - 1 - nondet_range(1000) as Ms;

    let a: ClientId = 0; // watches while expired  -> watched_expired = true
    let b: ClientId = 1; // watches after it is live -> watched_expired = false
    watch_key(&mut w, a, k);
    cvlr_assert!(w.clients[a].watched_expired[k]);

    // Make the key live again WITHOUT going through the lazy-expire delete, which is the
    // only way to keep a stale `expired` flag alongside a present key.
    w.slots[k].present = true;
    w.slots[k].expire_at = NO_EXPIRE;

    watch_key(&mut w, b, k);
    cvlr_assert!(!w.clients[b].watched_expired[k]);

    // Now modify the key. B watched it live and must be invalidated.
    key_modified(&mut w, k);

    clog!(k);
    cvlr_assert!(w.clients[b].dirty_cas);
}

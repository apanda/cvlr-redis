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
    dispatch(&mut w, writer, Cmd::Set { key: k, val: v, cond: SetCond::Always, ttl: TtlArg::None, get: false });

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

/// property: P-13. Execution-Unit-Framing.
/// description: a unit's propagated effects are wrapped in MULTI/EXEC iff the unit emitted
///   more than one op. One op goes bare; zero ops propagate nothing.
/// evidence: server.c:3993-4045 propagatePendingCommands; server.c:4005
///   `transaction_target = numops > 1 ? targets : PROPAGATE_NONE`.
/// oracle: tests/unit/multi.tcl:398 (unframed) vs :412 (framed); confirmed directly against
///   redis-server by `propagation_framing_matches_real_redis`.
/// status: unproven (differentially tested)
#[rule]
pub fn multi_unit_framed_iff_multi_op() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();
    let c = draw_client();
    let before = w.repl.len;

    dispatch(&mut w, c, draw_cmd(c));

    let n = w.repl.len - before;
    clog!(n);
    if n == 1 {
        // A single op is never framed.
        cvlr_assert!(w.repl.get(before) != Some(Effect::Multi));
        cvlr_assert!(w.repl.get(before) != Some(Effect::Exec));
    } else if n > 1 {
        // Framed: MULTI, at least two ops, EXEC.
        cvlr_assert!(w.repl.get(before) == Some(Effect::Multi));
        cvlr_assert!(w.repl.get(w.repl.len - 1) == Some(Effect::Exec));
        cvlr_assert!(n >= 4);
    }
}

/// property: P-14. Exec-Outcome-Is-Determined-By-Queue-Time-Errors.
/// description: EXEC returns -EXECABORT iff a command failed at QUEUE time. A runtime
///   error inside the block does NOT abort it -- it appears as an element of the reply
///   array while the other sub-commands still execute.
/// evidence: multi.c:110-125 (execCommandAbort / CLIENT_DIRTY_EXEC), multi.c:184-238.
/// status: unproven (differentially tested)
#[rule]
pub fn multi_exec_aborts_only_on_queue_time_error() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();
    let c = draw_client();

    dispatch(&mut w, c, Cmd::Multi);
    dispatch(&mut w, c, draw_cmd(c));

    let bad = draw_bool();
    if bad {
        dispatch(&mut w, c, Cmd::BadCommand);
    }

    let r = dispatch(&mut w, c, Cmd::Exec);
    clog!(bad);
    if bad {
        cvlr_assert!(r == Reply::ExecAborted);
    } else {
        cvlr_assert!(r != Reply::ExecAborted);
    }
    // Either way the block is closed afterwards.
    cvlr_assert!(!w.clients[c].in_multi);
}

/// property: P-15. Watch-Invalidation-Aborts-Exec.
/// description: if another client writes a WATCHed key between WATCH and EXEC, EXEC
///   returns the RESP null array and NONE of the queued commands take effect.
/// evidence: multi.c:387-425 (touchWatchedKey), multi.c:184-238 (CLIENT_DIRTY_CAS).
/// note: scoped to a key that is LIVE at WATCH time. The already-logically-expired case is
///   FINDINGS.md F-01 and is deliberately excluded here rather than silently folded in.
/// status: unproven (differentially tested)
#[rule]
pub fn multi_watch_conflict_aborts_exec() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();

    let k = draw_key();
    // Live at WATCH time: no TTL, so `watched_expired` is false and F-01 cannot apply.
    w.slots[k].present = true;
    w.slots[k].value = Value::Str(draw_str());
    w.slots[k].expire_at = NO_EXPIRE;

    let watcher: ClientId = 0;
    let other: ClientId = 1;

    dispatch(&mut w, watcher, Cmd::Watch { client: watcher, key: k });
    cvlr_assert!(!w.clients[watcher].watched_expired[k]);

    dispatch(&mut w, watcher, Cmd::Multi);
    dispatch(&mut w, watcher, Cmd::Get { key: k });

    let before = w.slots[k].value;
    dispatch(&mut w, other, Cmd::Set {
        key: k, val: draw_str(), cond: SetCond::Always, ttl: TtlArg::None, get: false,
    });

    let r = dispatch(&mut w, watcher, Cmd::Exec);
    let _ = before;
    cvlr_assert!(r == Reply::ExecNil);
}

/// property: P-16. Exec-Sees-One-Frozen-Clock.
/// description: the clock does not advance between the sub-commands of one EXEC, so a TTL
///   cannot expire mid-transaction.
/// evidence: server.c:1420-1432 -- `server.cmd_time_snapshot` is frozen at nesting 0.
/// note: in this model the property holds BY CONSTRUCTION (only `Step::ClockTick` moves the
///   clock, and it cannot occur inside a unit). The rule is a regression guard: if anyone
///   later makes a command read a live clock, it fails here.
/// status: unproven
#[rule]
pub fn multi_exec_clock_is_frozen() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();
    let c = draw_client();

    dispatch(&mut w, c, Cmd::Multi);
    dispatch(&mut w, c, draw_cmd(c));
    dispatch(&mut w, c, draw_cmd(c));

    let t0 = w.clock;
    dispatch(&mut w, c, Cmd::Exec);
    cvlr_assert!(w.clock == t0);
}

//! property: P-01. String-Set-Integrity.
//! property: P-06. Set-TTL-Argument-Semantics.

use cvlr::prelude::*;

use crate::model::{cmd::*, state::*, step::*};

/// property: P-01. String-Set-Integrity.
/// description: after an unconditional `SET k v` with no TTL argument, a subsequent read
///   of k yields v, k carries no TTL, and no other key is disturbed.
/// status: unproven
#[rule]
pub fn string_set_integrity() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();
    let k = draw_key();
    let v = draw_str();
    let pre = w;

    let r = dispatch(&mut w, 0, Cmd::Set { key: k, val: v, cond: SetCond::Always, ttl: TtlArg::None, get: false });

    clog!(k);
    cvlr_assert!(r == Reply::Ok);
    cvlr_assert!(w.slots[k].present);
    cvlr_assert!(w.slots[k].value == Value::Str(v));
    // A plain SET clears any existing TTL (t_string.c).
    cvlr_assert!(!w.slots[k].has_ttl());

    // No other key is disturbed -- physically, not just visibly.
    let mut j = 0;
    while j < K {
        if j != k {
            cvlr_assert!(w.slots[j].present == pre.slots[j].present);
            cvlr_assert!(w.slots[j].expire_at == pre.slots[j].expire_at);
        }
        j += 1;
    }
}

/// property: P-06. Set-TTL-Argument-Semantics.
/// description: `SET ... KEEPTTL` preserves an existing TTL; `SET` with no TTL argument
///   clears it; `SET ... PXAT t` installs exactly t WHEN t is in the future -- an
///   already-elapsed t writes nothing and deletes any existing key.
/// status: unproven
#[rule]
pub fn string_set_ttl_argument() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();
    let k = draw_key();
    let v = draw_str();

    // Only meaningful when the key is logically live beforehand.
    let pre_present = w.slots[k].present && !crate::model::expire::key_is_expired(&w, k);
    let pre_expire = w.slots[k].expire_at;

    let ttl = draw_ttl_arg();
    dispatch(&mut w, 0, Cmd::Set { key: k, val: v, cond: SetCond::Always, ttl, get: false });

    match ttl {
        TtlArg::None => cvlr_assert!(w.slots[k].expire_at == NO_EXPIRE),
        TtlArg::PxAt(at) => {
            // An ALREADY-ELAPSED absolute expire is not installed at all: the value is
            // never written and any existing key is deleted (t_string.c:161-175).
            // The naive "PXAT t installs t" was the original statement of P-06 and it is
            // FALSE -- caught by differential testing, not by reading the C.
            if at <= w.clock {
                cvlr_assert!(!w.slots[k].present);
            } else {
                cvlr_assert!(w.slots[k].expire_at == at);
            }
        }
        TtlArg::Keep => {
            if pre_present {
                cvlr_assert!(w.slots[k].expire_at == pre_expire);
            } else {
                cvlr_assert!(w.slots[k].expire_at == NO_EXPIRE);
            }
        }
    }
}

/// property: P-01b. Set-NX-XX-Respects-Logical-Existence.
/// description: SET NX succeeds iff the key is logically absent (i.e. after expiry, not
///   merely physically absent); SET XX is its complement.
/// status: unproven
#[rule]
pub fn string_set_nx_xx_uses_logical_existence() {
    let mut w = World::nondet_world();
    w.pin_standalone_master();
    let k = draw_key();
    let v = draw_str();

    // Logical existence at the moment of the command.
    let logically_present = w.slots[k].present && !crate::model::expire::key_is_expired(&w, k);
    let pre_val = w.slots[k].value;

    let nx = draw_bool();
    let cond = if nx { SetCond::Nx } else { SetCond::Xx };
    let r = dispatch(&mut w, 0, Cmd::Set { key: k, val: v, cond, ttl: TtlArg::None, get: false });

    let applied = r == Reply::Ok;
    clog!(logically_present);
    if nx {
        cvlr_assert!(applied == !logically_present);
    } else {
        cvlr_assert!(applied == logically_present);
    }
    if !applied {
        // A rejected SET must not change the value. (It MAY have deleted a logically
        // expired key as a side effect of the lookup -- that is why this checks the value
        // only when the key survived.)
        if w.slots[k].present {
            cvlr_assert!(w.slots[k].value == pre_val);
        }
    }
}

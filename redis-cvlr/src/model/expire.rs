//! Expiry: a faithful transcription of `keyIsExpired` and `expireIfNeeded`.
//!
//! This is the module the whole specification turns on, and it is the one every informal
//! account of Redis gets wrong. The headline folklore -- "a key past its TTL is never
//! observed" -- is FALSE as an unconditional statement. It holds only for the
//! `lookupKey*` path.
//!
//! Transcribed from redis/src/db.c:2946-2073 (`keyIsExpired`) and db.c:3004-3073
//! (`expireIfNeeded`) as of 8.9.241. Every branch below cites its line.

use super::state::*;

/// Mirrors `keyStatus` (redis/src/db.c:39-45).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyStatus {
    /// Key is valid (or does not exist).
    Valid,
    /// Logically expired, but deliberately NOT deleted here.
    Expired,
    /// Logically expired and physically removed by this call.
    Deleted,
}

/// Flags to `expireIfNeeded`. Mirrors the `EXPIRE_*` constants in db.c.
#[derive(Clone, Copy, Default)]
pub struct ExpireFlags {
    /// `EXPIRE_ALLOW_ACCESS_EXPIRED` (db.c:3022).
    pub allow_access_expired: bool,
    /// `EXPIRE_FORCE_DELETE_EXPIRED` (db.c:3044, 3050) -- set on write lookups.
    pub force_delete_expired: bool,
    /// `EXPIRE_AVOID_DELETE_EXPIRED` (db.c:3055).
    pub avoid_delete_expired: bool,
}

impl ExpireFlags {
    pub fn read() -> Self {
        ExpireFlags::default()
    }
    /// `lookupKeyWrite` forces deletion so writes never operate on a stale value.
    pub fn write() -> Self {
        ExpireFlags { force_delete_expired: true, ..Default::default() }
    }
}

/// `keyIsExpired` -- db.c:2946-2955.
///
/// ```text
/// if (server.loading || server.allow_access_expired) return 0;
/// mstime_t when = getExpire(db, key, kv);
/// if (when < 0) return 0;
/// const mstime_t now = commandTimeSnapshot();
/// return now > when;              <-- STRICT
/// ```
///
/// The comparison is STRICT (`now > when`). The active expire cycle uses
/// `if (now < expire) return 0;` i.e. expires at `now >= when` (expire.c:40-41). At
/// exactly `now == when` a lazy lookup sees the key as VALID while the active cycle
/// deletes it. Hash-field TTL uses a third comparison (`expiredAt < now`,
/// t_hash.c:2118-2120). A model that picks one comparison disagrees with the others.
pub fn key_is_expired(w: &World, k: KeyId) -> bool {
    let k = kidx(k); // provably in range: see `World` -- keeps panic_bounds_check out of the wasm
    if w.loading || w.allow_access_expired {
        return false; // db.c:2948
    }
    let when = w.slots[k].expire_at;
    if when < 0 {
        // db.c:2950 -- `if (when < 0) return 0;`. ANY negative means "no expire", not just
        // the -1 sentinel. Testing `== NO_EXPIRE` here made a stray negative timestamp read
        // as "expired in the distant past", the opposite of Redis.
        return false;
    }
    w.clock > when // db.c:2954 -- STRICT
}

/// The active expire cycle's comparison -- expire.c:40-41. Deliberately distinct from
/// `key_is_expired`; the one-millisecond disagreement is a real, pinned property.
pub fn active_cycle_would_expire(w: &World, k: KeyId) -> bool {
    let k = kidx(k);
    let when = w.slots[k].expire_at;
    if when < 0 {
        return false;
    }
    w.clock >= when
}

/// `expireIfNeeded` -- db.c:3004-3073.
///
/// SEVEN paths return without deleting, several of which fire on a plain standalone
/// master. Enumerated here in source order; `asmIsKeyInTrimJob` (db.c:3011-3019) is the
/// eighth and is modeled as always-false because atomic slot migration is out of scope.
pub fn expire_if_needed(w: &mut World, k: KeyId, flags: ExpireFlags) -> KeyStatus {
    let k = kidx(k);
    // (1) db.c:3022 -- explicit caller opt-out, and (2) db.c:3023 -- not expired at all.
    if flags.allow_access_expired || !key_is_expired(w, k) {
        return KeyStatus::Valid;
    }

    // (3) db.c:3042-3045. NOTE the guard is `masterhost != NULL || cluster_enabled`, so
    // enabling cluster changes expiry behaviour even on a master.
    if w.role == Role::ReadOnlyReplica || w.cluster_enabled {
        // A command arriving FROM the master never sees keys as expired.
        if w.caller == Caller::MasterLink {
            return KeyStatus::Valid; // db.c:3043
        }
        if w.role == Role::ReadOnlyReplica && !flags.force_delete_expired {
            // The replica hides the key but does NOT delete it: expiry on a replica is
            // driven by synthesized DELs from the master. This is a genuine
            // master/replica observable divergence.
            return KeyStatus::Expired; // db.c:3044
        }
    }

    // (4) db.c:3050-3051 -- user config disables lazy-expire deletion.
    if !flags.force_delete_expired && !w.conf_allows_expire_del {
        return KeyStatus::Expired;
    }

    // (5) db.c:3055 -- caller wants "missing" reported without a delete, even on a master.
    if flags.avoid_delete_expired {
        return KeyStatus::Expired;
    }

    // (6) db.c:3062 -- expiry action paused (e.g. during failover or slot handoff).
    if w.expire_paused {
        return KeyStatus::Expired;
    }

    // db.c:3063-3072 -- perform deletion and propagate a bare DEL.
    delete_expired_key_and_propagate(w, k);
    KeyStatus::Deleted
}

/// `deleteExpiredKeyAndPropagate` -> `deleteKeyAndPropagate` (db.c:2898-2900).
///
/// The propagated DEL is a standalone op. It is deliberately NOT wrapped in MULTI/EXEC --
/// pinned by redis/tests/unit/expire.tcl:809 and :830.
pub fn delete_expired_key_and_propagate(w: &mut World, k: KeyId) {
    let k = kidx(k);
    w.slots[k] = Slot::absent();
    w.repl.push(Effect::Del { key: k });
    w.dirty += 1;
    // A physical delete of a watched key dirties the CAS of every watcher -- EXCEPT the
    // watchers that saw it already logically expired at WATCH time (multi.c:397-406).
    super::watch::touch_watched_key(w, k, /* from_expiry = */ true);
}

/// `lookupKeyRead` -- the read path. Returns the value only if the key survives
/// `expire_if_needed`.
pub fn lookup_key_read(w: &mut World, k: KeyId) -> Option<Value> {
    let k = kidx(k);
    if !w.slots[k].present {
        return None;
    }
    match expire_if_needed(w, k, ExpireFlags::read()) {
        KeyStatus::Valid => Some(w.slots[k].value),
        // Both Expired and Deleted are INVISIBLE to the caller. The difference is whether
        // the key is still physically there -- which DBSIZE can see and GET cannot.
        KeyStatus::Expired | KeyStatus::Deleted => None,
    }
}

/// `lookupKeyWrite` -- forces deletion so a write never lands on a stale value.
pub fn lookup_key_write(w: &mut World, k: KeyId) -> Option<Value> {
    let k = kidx(k);
    if !w.slots[k].present {
        return None;
    }
    match expire_if_needed(w, k, ExpireFlags::write()) {
        KeyStatus::Valid => Some(w.slots[k].value),
        KeyStatus::Expired | KeyStatus::Deleted => None,
    }
}

/// An adversarial active-expire step. Not a background detail to be assumed away: it is a
/// `Step` the prover may insert anywhere, which turns "a key may vanish at any moment"
/// from prose into a hypothesis every other rule must survive.
///
/// On a replica the cycle does not delete (expire.c) -- expiry is master-driven.
pub fn active_expire_cycle(w: &mut World) {
    if w.role == Role::ReadOnlyReplica || w.expire_paused {
        return;
    }
    let mut i = 0;
    while i < K {
        if w.slots[i].present && active_cycle_would_expire(w, i) {
            delete_expired_key_and_propagate(w, i);
        }
        i += 1;
    }
}

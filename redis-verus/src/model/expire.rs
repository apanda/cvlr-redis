//! Expiry: a transcription of `keyIsExpired` and `expireIfNeeded`.
//!
//! From redis/src/db.c:2946-2955 (`keyIsExpired`) and db.c:3004-3073 (`expireIfNeeded`)
//! as of 8.9.241. Mirrors ../../redis-cvlr/src/model/expire.rs; the two legs must agree,
//! and the differential driver is what keeps them honest.

use vstd::prelude::*;
use crate::model::state::*;

verus! {

/// Mirrors `keyStatus` (redis/src/db.c:39-45).
#[derive(PartialEq, Eq, Structural, Clone, Copy)]
pub enum KeyStatus {
    /// Valid, or does not exist.
    Valid,
    /// Logically expired, but deliberately NOT deleted here.
    Expired,
    /// Logically expired and physically removed by this call.
    Deleted,
}

/// `keyIsExpired` -- db.c:2946-2955.
///
/// ```text
/// if (server.loading || server.allow_access_expired) return 0;
/// mstime_t when = getExpire(db, key, kv);
/// if (when < 0) return 0;              <-- ANY negative, not just the -1 sentinel
/// const mstime_t now = commandTimeSnapshot();
/// return now > when;                   <-- STRICT
/// ```
pub open spec fn key_is_expired(w: World, k: int) -> bool {
    if w.loading || w.allow_access_expired {
        false                                       // db.c:2948
    } else if w.slots@[k].expire_at < 0 {
        false                                       // db.c:2950
    } else {
        w.clock > w.slots@[k].expire_at             // db.c:2954 -- STRICT
    }
}

/// The active expire cycle's comparison -- expire.c:40-41. Deliberately distinct from
/// `key_is_expired`: at exactly `clock == expire_at` the lazy path sees the key VALID
/// while the active cycle deletes it. That one-millisecond disagreement is P-03.
pub open spec fn active_cycle_would_expire(w: World, k: int) -> bool {
    w.slots@[k].expire_at >= 0 && w.clock >= w.slots@[k].expire_at
}

/// `expireIfNeeded` -- db.c:3004-3073, as a STATUS. Seven paths return without deleting;
/// several fire on a plain standalone master. `asmIsKeyInTrimJob` (db.c:3011-3019) is
/// modelled as always-false -- atomic slot migration is out of scope.
///
/// `is_write` selects `lookupKeyWrite`'s EXPIRE_FORCE_DELETE_EXPIRED.
pub open spec fn expire_status(w: World, k: int, is_write: bool) -> KeyStatus {
    if !key_is_expired(w, k) {
        KeyStatus::Valid                                    // db.c:3022-3023
    } else if (w.role == Role::ReadOnlyReplica || w.cluster_enabled)
              && w.caller == Caller::MasterLink {
        KeyStatus::Valid                                    // db.c:3043
    } else if w.role == Role::ReadOnlyReplica && !is_write {
        KeyStatus::Expired                                  // db.c:3044 -- hides, no delete
    } else if !is_write && !w.conf_allows_expire_del {
        KeyStatus::Expired                                  // db.c:3050-3051
    } else if w.expire_paused {
        KeyStatus::Expired                                  // db.c:3062
    } else {
        KeyStatus::Deleted                                  // db.c:3063-3072
    }
}

/// Executable `keyIsExpired`.
pub fn key_is_expired_exec(w: &World, k: usize) -> (b: bool)
    requires w.valid_key(k as int),
    ensures b == key_is_expired(*w, k as int),
{
    if w.loading || w.allow_access_expired {
        false
    } else if w.slots[k].expire_at < 0 {
        false
    } else {
        w.clock > w.slots[k].expire_at
    }
}

/// Executable `expireIfNeeded` status.
pub fn expire_status_exec(w: &World, k: usize, is_write: bool) -> (s: KeyStatus)
    requires w.valid_key(k as int),
    ensures s == expire_status(*w, k as int, is_write),
{
    if !key_is_expired_exec(w, k) {
        KeyStatus::Valid
    } else if (w.role == Role::ReadOnlyReplica || w.cluster_enabled)
              && w.caller == Caller::MasterLink {
        KeyStatus::Valid
    } else if w.role == Role::ReadOnlyReplica && !is_write {
        KeyStatus::Expired
    } else if !is_write && !w.conf_allows_expire_del {
        KeyStatus::Expired
    } else if w.expire_paused {
        KeyStatus::Expired
    } else {
        KeyStatus::Deleted
    }
}

} // verus!

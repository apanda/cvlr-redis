//! Abstract Redis state -- EXECUTABLE.
//!
//! DESIGN DECISION 1 (same as the CVLR leg): the keyspace is PHYSICAL. `Slot::present`
//! mirrors membership in `db->keys`; whether a client can SEE a key is derived, not a
//! field. Forced by the C -- `dbSize` returns `kvstoreSize(db->keys)` with no expiry
//! filter (redis/src/db.c:3148-3173), while `EXISTS`/`GET` reach the key through
//! `lookupKey*` and DO delete.
//!
//! DESIGN DECISION 2: the propagated effect stream is part of observable state.
//!
//! Exec types (`Vec`, `i64`), not ghost types (`Seq`, `int`), so the same model can be
//! compiled by `verus --compile` and driven by the differential harness. Specs read the
//! `@` views. Unlike the CVLR leg there is no `K = 3` and no `REPL_CAP`: both are
//! unbounded, and proofs go by induction or loop invariant rather than unrolling.

use vstd::prelude::*;

verus! {

/// No TTL. Mirrors `kvobjGetExpire(kv) == -1` (redis/src/object.h).
pub const NO_EXPIRE: i64 = -1;

#[derive(PartialEq, Eq, Structural, Clone, Copy)]
pub struct Slot {
    pub present: bool,
    pub expire_at: i64,
}

/// Who is issuing the command. `MasterLink` is the replication-link client
/// (`CLIENT_MASTER`), for which keys are NEVER considered expired (db.c:3043).
#[derive(PartialEq, Eq, Structural, Clone, Copy)]
pub enum Caller { Normal, MasterLink }

#[derive(PartialEq, Eq, Structural, Clone, Copy)]
pub enum Role { Master, ReadOnlyReplica }

/// A propagated effect, as it appears on the wire to a replica / in the AOF.
/// The model's analogue of `assert_replication_stream`.
#[derive(PartialEq, Eq, Structural, Clone, Copy)]
pub enum Effect {
    Multi,
    Exec,
    Del { key: usize },
}

pub struct World {
    pub slots: Vec<Slot>,
    /// Frozen for a whole execution unit (`server.cmd_time_snapshot`, server.c:1420-1432).
    pub clock: i64,
    pub role: Role,
    pub caller: Caller,
    /// `server.loading` (db.c:2948).
    pub loading: bool,
    /// `server.allow_access_expired` (db.c:2948) -- DEBUG SET-ALLOW-ACCESS-EXPIRED.
    pub allow_access_expired: bool,
    /// `isPausedActionsWithUpdate(PAUSE_ACTION_EXPIRE)` (db.c:3062).
    pub expire_paused: bool,
    /// `server.cluster_enabled`. The replica escape in `expireIfNeeded` is guarded by
    /// `masterhost != NULL || cluster_enabled`, so cluster mode ALONE changes expiry
    /// behaviour on a master (db.c:3042).
    pub cluster_enabled: bool,
    /// `confAllowsExpireDel()`, gated on `lazyexpire-nested-arbitrary-keys` (db.c:3050).
    pub conf_allows_expire_del: bool,
    pub repl: Vec<Effect>,
}

impl World {
    pub open spec fn valid_key(self, k: int) -> bool { 0 <= k < self.slots@.len() }

    /// The iteration-1 scope fence: standalone master, normal client, no debug escapes,
    /// no pause, cluster off.
    pub open spec fn standalone_master(self) -> bool {
        &&& self.role == Role::Master
        &&& self.caller == Caller::Normal
        &&& self.loading == false
        &&& self.allow_access_expired == false
        &&& self.expire_paused == false
        &&& self.cluster_enabled == false
        &&& self.conf_allows_expire_del == true
    }
}

/// Well-formedness. P-05 in the catalog: no ABSENT slot carries a TTL.
///
/// In 8.9.241 this is structural in the C -- the value is a `kvobj` embedding both the key
/// and the expire, and `db->keys` / `db->expires` are kvstores over the SAME pointers
/// (object.h:31-66). The model must preserve it or it has stopped modelling Redis.
pub open spec fn wf(w: World) -> bool {
    forall|j: int| #![auto] 0 <= j < w.slots@.len()
        ==> (w.slots@[j].present || w.slots@[j].expire_at == NO_EXPIRE)
}

/// The configuration frame: everything a keyspace command must leave alone.
pub open spec fn same_config(a: World, b: World) -> bool {
    &&& a.clock == b.clock
    &&& a.role == b.role
    &&& a.caller == b.caller
    &&& a.loading == b.loading
    &&& a.allow_access_expired == b.allow_access_expired
    &&& a.expire_paused == b.expire_paused
    &&& a.cluster_enabled == b.cluster_enabled
    &&& a.conf_allows_expire_del == b.conf_allows_expire_del
}

} // verus!

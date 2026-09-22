//! Abstract Redis state.
//!
//! DESIGN DECISION 1 (load-bearing): the keyspace is PHYSICAL. `Slot::present` mirrors
//! membership in `db->keys`. Whether a client can *see* a key is a DERIVED function
//! (`visible()`), parameterized by role, caller and config -- it is not a field.
//!
//! This is forced by the C. `dbSize` returns `kvstoreSize(db->keys)` with no expiry
//! filter at all (redis/src/db.c:3148-3173); `KEYS` filters with `keyIsExpired` but does
//! NOT delete (db.c:1638-1645); `EXISTS`/`GET` reach the key through `lookupKey*` and DO
//! delete. Measured on redis-server 8.9.241 with one elapsed `PX`:
//!   DBSIZE -> 1, INFO db0:keys=1, KEYS * -> [] (DBSIZE stays 1), EXISTS -> 0 (DBSIZE -> 0).
//! A model with a single `HashMap<Key, Value>` cannot state that at all.
//!
//! DESIGN DECISION 2: the propagated effect stream (`ReplLog`) is part of observable
//! state. That is what turns "atomic" from an adjective into a checkable predicate, and
//! it gives every such property a free differential twin against Redis's own
//! `assert_replication_stream` (tests/test_helper.tcl:787-864).

use cvlr::nondet::nondet;

/// Bounds. Keep tiny: `optimistic_loop` + `loop_iter` in the conf silently assume away
/// every execution that iterates past the bound, so "for all keys" means "for all K".
pub const K: usize = 3; // distinct keys
pub const C: usize = 2; // clients
pub const VLEN: usize = 8; // max string value length
pub const REPL_CAP: usize = 4; // bounded propagation log
//
// REPL_CAP is 4, not 8, because `ReplLog` is copied whole by every `let pre = w` and
// dominated `World`'s footprint (328 of 592 bytes). The Prover has to scalarize those
// copies; the sqlite-demo Sunbeam run logs `failed to scalarize memory accesses` on a
// module a fraction of this size. No rule inspects more than 3 effects, so raise this
// only alongside the rule that needs it.

pub type KeyId = usize;
pub type ClientId = usize;
pub type Ms = i64;

/// No TTL. Mirrors `kvobjGetExpire(kv) == -1` (redis/src/object.h).
pub const NO_EXPIRE: Ms = -1;

// ---------------------------------------------------------------- time windows
//
// Two rules govern every timestamp drawn anywhere in this crate.
//
// 1. NEVER DRAW A NEGATIVE TIMESTAMP. Redis treats any `when < 0` as "no expire"
//    (db.c:2950), and NO_EXPIRE is -1. A rule that backdates a key with
//    `clock - 1 - offset` and happens to land on -1 gets the EXACT OPPOSITE of what it
//    intended -- the key reads as having no TTL at all. `CLOCK_BASE > PAST_MAX` makes that
//    unreachable by construction rather than by assumption.
//
//    This is not hypothetical: with the previous window (`clock` in [0, 10^6), offset in
//    [0, 10^3)) the collision had probability ~1e-6 per execution. 20 000 concrete runs
//    never hit it; an SMT solver finds it on the first query.
//
// 2. SPANS ARE POWERS OF TWO. Every draw is `nondet::<u64>() % SPAN`, and `urem` by a
//    power of two is a mask. A remainder by 1_000_000 on a fresh symbolic u64 is a real
//    cost for the SMT backend, for no modeling benefit.

/// Clocks are drawn from `[CLOCK_BASE, CLOCK_BASE + CLOCK_SPAN)`.
pub const CLOCK_BASE: Ms = 1 << 20;
pub const CLOCK_SPAN: u64 = 1 << 20;
/// The largest backdate any rule applies. MUST stay below `CLOCK_BASE` -- see rule 1.
pub const PAST_MAX: u64 = 1 << 10;
/// The largest forward offset any rule applies.
pub const FUTURE_MAX: u64 = 1 << 11;
/// Span for an arbitrary absolute-timestamp ARGUMENT. Straddles the clock window, so both
/// already-elapsed and still-future arguments stay reachable.
pub const TS_SPAN: u64 = 1 << 21;

// ---------------------------------------------------------------- values

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Str {
    pub bytes: [u8; VLEN],
    pub len: usize,
}

impl Str {
    pub fn empty() -> Self {
        Str { bytes: [0u8; VLEN], len: 0 }
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// Only STRING in iteration 1. Aggregates land with the no-empty-collection invariant.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Str(Str),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VType {
    Str,
}

impl Value {
    pub fn vtype(&self) -> VType {
        match self {
            Value::Str(_) => VType::Str,
        }
    }
}

// ---------------------------------------------------------------- keyspace

/// One keyspace entry. `present` is PHYSICAL membership in `db->keys`.
///
/// In 8.9.241 the value is a `kvobj` embedding the key AND the expire timestamp as object
/// metadata (redis/src/object.h:31-66), and `db->keys` / `db->expires` are kvstores over
/// the SAME pointers. So `key in expires <=> expire != NO_EXPIRE` is structural here, not
/// an invariant to prove -- we get it free by storing the expire in the slot.
#[derive(Clone, Copy)]
pub struct Slot {
    pub present: bool,
    pub value: Value,
    pub expire_at: Ms,
}

impl Slot {
    pub fn absent() -> Self {
        Slot { present: false, value: Value::Str(Str::empty()), expire_at: NO_EXPIRE }
    }
    /// Redis's test is `when < 0` (db.c:2950), not `when == -1`. Keep them the same test
    /// here so a stray negative timestamp cannot mean "expired long ago" in one place and
    /// "no TTL" in another.
    pub fn has_ttl(&self) -> bool {
        self.expire_at >= 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Master,
    ReadOnlyReplica,
}

/// Who is issuing the command. `MasterLink` is the replication-link client
/// (`CLIENT_MASTER`), for which keys are NEVER considered expired (db.c:3044).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    Normal,
    MasterLink,
}

// ---------------------------------------------------------------- effects

/// A propagated effect, as it appears on the wire to a replica / in the AOF.
/// This is the model's analogue of `assert_replication_stream`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Multi,
    Exec,
    /// `SET k v` with an ABSOLUTE expire. Redis rewrites relative TTLs to `PXAT` so the
    /// log replays identically against a different clock (t_string.c:178-195).
    Set { key: KeyId, val: Str, pxat: Ms },
    Del { key: KeyId },
    PExpireAt { key: KeyId, at: Ms },
    Persist { key: KeyId },
}

#[derive(Clone, Copy)]
pub struct ReplLog {
    pub ops: [Option<Effect>; REPL_CAP],
    pub len: usize,
}

impl ReplLog {
    /// `const` initializer ON PURPOSE. Written inline, `[None; REPL_CAP]` compiles to an
    /// 8-iteration store loop (`Option<Effect>` is 40 bytes, so it cannot be a memset).
    /// That exceeds the `loop_iter` in the confs, and with `optimistic_loop: true` the
    /// Prover ASSUMES AWAY every execution that runs past the bound -- which would make
    /// every rule vacuous, since every rule builds a `World`. As a `const` the array is
    /// materialized in the data section and copied, leaving no loop to bound.
    pub fn new() -> Self {
        const EMPTY: [Option<Effect>; REPL_CAP] = [None; REPL_CAP];
        ReplLog { ops: EMPTY, len: 0 }
    }
    pub fn push(&mut self, e: Effect) {
        if self.len < REPL_CAP {
            self.ops[self.len] = Some(e);
            self.len += 1;
        }
    }
    pub fn get(&self, i: usize) -> Option<Effect> {
        // `% REPL_CAP` is a no-op given `push`'s guard, but it makes the index provably
        // in range so no `panic_bounds_check` edge survives into the wasm. See the note on
        // `World::slot`.
        if i < self.len { self.ops[i % REPL_CAP] } else { None }
    }
}

// ---------------------------------------------------------------- clients

#[derive(Clone, Copy)]
pub struct ClientState {
    /// WATCHed keys, by id.
    pub watching: [bool; K],
    /// Whether the key was ALREADY logically expired when WATCH ran. Mirrors `wk->expired`
    /// (multi.c:330) -- the exemption that makes naive CAS models wrong.
    pub watched_expired: [bool; K],
    pub dirty_cas: bool,
    pub in_multi: bool,
    pub dirty_exec: bool,
}

impl ClientState {
    pub fn new() -> Self {
        ClientState {
            watching: [false; K],
            watched_expired: [false; K],
            dirty_cas: false,
            in_multi: false,
            dirty_exec: false,
        }
    }
}

// ---------------------------------------------------------------- world

/// Per-key ORDERED watcher list, mirroring `db->watched_keys` mapping a key to a `list*`
/// of `watchedKey` in insertion order (`listAddNodeTail`, multi.c:332).
///
/// The order is load-bearing, not an implementation detail: `touchWatchedKey` contains a
/// `break` (multi.c:415) that abandons the REST of the list, so which watchers get dirtied
/// depends on their position. A model keyed only by client id cannot express that.
#[derive(Clone, Copy)]
pub struct WatchTable {
    pub order: [[Option<ClientId>; C]; K],
    pub len: [usize; K],
}

impl WatchTable {
    pub fn new() -> Self {
        const EMPTY: [[Option<ClientId>; C]; K] = [[None; C]; K];
        WatchTable { order: EMPTY, len: [0usize; K] }
    }
    /// `listAddNodeTail` (multi.c:332).
    pub fn add_tail(&mut self, k: KeyId, c: ClientId) {
        let k = kidx(k);
        if self.len[k] < C {
            self.order[k][self.len[k]] = Some(c);
            self.len[k] += 1;
        }
    }
    pub fn remove(&mut self, k: KeyId, c: ClientId) {
        let k = kidx(k);
        let mut i = 0;
        let mut w = 0;
        let n = self.len[k];
        while i < n {
            if self.order[k][cidx(i)] != Some(c) {
                self.order[k][cidx(w)] = self.order[k][cidx(i)];
                w += 1;
            }
            i += 1;
        }
        let mut j = w;
        while j < C {
            self.order[k][cidx(j)] = None;
            j += 1;
        }
        self.len[k] = w;
    }
}

#[derive(Clone, Copy)]
pub struct World {
    pub slots: [Slot; K],
    pub watched: WatchTable,
    /// Frozen for a whole execution unit (`server.cmd_time_snapshot`, server.c:1420-1432).
    pub clock: Ms,
    pub role: Role,
    pub caller: Caller,
    /// `server.loading` (db.c:2948). Pinned false by the iteration-1 fence; carried as a
    /// field so `key_is_expired` is a COMPLETE transcription of `keyIsExpired` rather than
    /// a partial one. P-02b needs it.
    pub loading: bool,
    /// `server.allow_access_expired` (db.c:2948) -- DEBUG SET-ALLOW-ACCESS-EXPIRED.
    pub allow_access_expired: bool,
    /// `isPausedActionsWithUpdate(PAUSE_ACTION_EXPIRE)` (db.c:3062).
    pub expire_paused: bool,
    /// `server.cluster_enabled`. NOTE: the replica escape in `expireIfNeeded` is guarded by
    /// `server.masterhost != NULL || server.cluster_enabled`, so cluster mode ALONE changes
    /// expiry behaviour on a master (db.c:3042).
    pub cluster_enabled: bool,
    /// `confAllowsExpireDel()`, gated on the `lazyexpire-nested-arbitrary-keys` config
    /// (db.c:3050-3051).
    pub conf_allows_expire_del: bool,
    pub clients: [ClientState; C],
    pub repl: ReplLog,
    /// `server.dirty` delta within the current execution unit.
    pub dirty: u64,
    /// `server.execution_nesting` (server.c:1420-1432).
    pub nesting: u32,
}

// ---------------------------------------------------------------- nondet
//
// DRAW, DO NOT FILTER. `nondet::<u64>() % K` keeps these same bodies usable in the
// concrete/fuzzing driver; `cvlr_assume!(k < K)` rejects essentially every concrete run.

/// MEASURED CONSTRAINT: never emit a remainder by a non-power-of-two constant.
///
/// `nondet::<u64>() % 3` puts an `i64.rem_u` by 3 on a fresh symbolic word, and the Prover
/// cannot discharge it. In the milestone-0 bisect the correlation was exact: all four
/// smoke rules WITHOUT such a remainder verified in 0 s, and all three WITH one came back
/// Violated or UNKNOWN -- including two rules that are true by construction and that the
/// concrete driver executes 20 000/20 000 times. `smoke_const_index` and
/// `smoke_expect_verified` differ ONLY in this.
///
/// A mask plus a fold keeps the draw total, keeps every key reachable, and keeps concrete
/// mode viable. The slight bias toward the last key is irrelevant to both modes.
pub fn draw_key() -> KeyId {
    match nondet::<u64>() & 3 {
        0 => 0,
        1 => 1,
        _ => K - 1,
    }
}

/// Clamp an index into range WITHOUT a remainder -- a compare-and-select, not an
/// `i64.rem_u`. Used at model-function entries so no `panic_bounds_check` edge survives.
pub fn kidx(k: KeyId) -> KeyId {
    if k < K { k } else { K - 1 }
}

pub fn cidx(c: ClientId) -> ClientId {
    if c < C { c } else { C - 1 }
}

pub fn draw_client() -> ClientId {
    // C = 2 is a power of two, so this is a mask; kept in this form for uniformity.
    (nondet::<u64>() & ((C as u64) - 1)) as usize
}

pub fn draw_bool() -> bool {
    nondet::<u64>() % 2 == 0
}

pub fn draw_len() -> usize {
    (nondet::<u64>() % ((VLEN + 1) as u64)) as usize
}

/// Draw one of a small set of DISTINCT string values.
///
/// Deliberately not arbitrary bytes. Two reasons, both load-bearing:
///
/// 1. COST. Every `nondet::<u64>()` is a full symbolic word. Drawing VLEN bytes per string
///    multiplies that by 8 for no benefit -- every property in iteration 1 compares values
///    for EQUALITY only, so a handful of distinct values is exactly as discriminating.
/// 2. LOOP BOUNDS. A `while i < VLEN` loop runs 8 iterations, which exceeds the
///    `loop_iter` in the confs. With `optimistic_loop: true` the Prover ASSUMES AWAY every
///    execution that iterates past the bound -- so a byte-filling loop would silently make
///    these rules vacuous rather than failing loudly. The array-repeat below compiles to a
///    memset with no loop for the Prover to bound.
///
/// This must change when APPEND / SETRANGE / GETRANGE land: those need real bytes, and
/// they will need `loop_iter` raised to at least VLEN + 1 with `optimistic_loop` off.
pub fn draw_str() -> Str {
    let id = (nondet::<u64>() % 4) as u8;
    Str { bytes: [id; VLEN], len: (id as usize) % (VLEN + 1) }
}

/// A clock bounded well away from i64 extremes so TTL arithmetic cannot overflow, and
/// bounded BELOW by `CLOCK_BASE` so backdating can never produce a negative timestamp.
pub fn draw_clock() -> Ms {
    CLOCK_BASE + (nondet::<u64>() % CLOCK_SPAN) as Ms
}

/// A timestamp strictly in the past of `clock`. Never negative, so never NO_EXPIRE.
/// Use this instead of writing `clock - 1 - nondet_range(n)` by hand.
pub fn draw_past(clock: Ms) -> Ms {
    clock - 1 - (nondet::<u64>() % PAST_MAX) as Ms
}

/// A timestamp strictly in the future of `clock`.
pub fn draw_future(clock: Ms) -> Ms {
    clock + 1 + (nondet::<u64>() % FUTURE_MAX) as Ms
}

/// An arbitrary absolute timestamp argument, always >= 0.
pub fn draw_ts() -> Ms {
    (nondet::<u64>() % TS_SPAN) as Ms
}

pub fn draw_expire_at() -> Ms {
    if draw_bool() { NO_EXPIRE } else { draw_ts() }
}

impl World {
    /// An arbitrary but WELL-FORMED world. Anything that is structural in the C (e.g.
    /// `expires` being a subset of `keys`) must be structural here too -- an absent slot
    /// carries no TTL.
    pub fn nondet_world() -> Self {
        let mut w = World {
            slots: [Slot::absent(); K],
            clock: draw_clock(),
            role: if draw_bool() { Role::Master } else { Role::ReadOnlyReplica },
            caller: if draw_bool() { Caller::Normal } else { Caller::MasterLink },
            loading: false,
            allow_access_expired: false,
            expire_paused: false,
            cluster_enabled: false,
            conf_allows_expire_del: true,
            clients: [ClientState::new(); C],
            watched: WatchTable::new(),
            repl: ReplLog::new(),
            dirty: 0,
            nesting: 0,
        };
        let mut i = 0;
        while i < K {
            let present = draw_bool();
            w.slots[i] = if present {
                Slot { present: true, value: Value::Str(draw_str()), expire_at: draw_expire_at() }
            } else {
                Slot::absent()
            };
            i += 1;
        }
        w
    }

    /// An EMPTY world at a given clock. Used by the differential driver, which starts
    /// from a freshly-flushed server and must therefore not start from a drawn state.
    pub fn empty(clock: Ms) -> Self {
        World {
            slots: [Slot::absent(); K],
            watched: WatchTable::new(),
            clock,
            role: Role::Master,
            caller: Caller::Normal,
            loading: false,
            allow_access_expired: false,
            expire_paused: false,
            cluster_enabled: false,
            conf_allows_expire_del: true,
            clients: [ClientState::new(); C],
            repl: ReplLog::new(),
            dirty: 0,
            nesting: 0,
        }
    }

    /// The default scope fence for iteration 1: standalone master, normal client, no
    /// debug escapes, no pause, cluster off. Rules that want the general case skip this.
    pub fn pin_standalone_master(&mut self) {
        self.role = Role::Master;
        self.caller = Caller::Normal;
        self.loading = false;
        self.allow_access_expired = false;
        self.expire_paused = false;
        self.cluster_enabled = false;
        self.conf_allows_expire_del = true;
    }

    /// Observable projection: the `csvdump`-shaped snapshot
    /// (redis/tests/support/util.tcl:496-570). Encoding, refcount, LRU and memory are
    /// ABSENT from the model, not hidden in it.
    pub fn snapshot_visible(&self) -> [Option<(VType, Str, Ms)>; K] {
        let mut out = [None; K];
        let mut i = 0;
        while i < K {
            if self.slots[i].present && !crate::model::expire::key_is_expired(self, i) {
                // `match`, not `if let`: exhaustive today, and it will refuse to compile
                // when List/Hash/Set/ZSet are added rather than silently dropping them.
                match self.slots[i].value {
                    Value::Str(s) => out[i] = Some((VType::Str, s, self.slots[i].expire_at)),
                }
            }
            i += 1;
        }
        out
    }
}

//! Commands and replies for iteration 1: strings + keyspace + expiry.
//!
//! Scope fence (see CLAUDE.md): standalone, cluster off, one DB, maxmemory=0,
//! appendonly no, no modules/Lua/ACL, no floats. The 8.9.241-only surface (DELEX,
//! HSETEX, BLESS, OBJ_ARRAY, ...) is deliberately deferred.

use super::expire::*;
use super::state::*;
use super::watch;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SetCond {
    Always,
    /// Only set if the key does not already exist.
    Nx,
    /// Only set if the key already exists.
    Xx,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TtlArg {
    /// No TTL argument: SET clears any existing TTL.
    None,
    /// `KEEPTTL`.
    Keep,
    /// `EX`/`PX`/`EXAT`/`PXAT`, already normalized to an absolute ms timestamp.
    PxAt(Ms),
}

/// `EXPIRE`'s optional flags (expire.c:739, `parseExtendedExpireArgumentsOrReply`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExpireCond {
    None,
    Nx,
    Xx,
    Gt,
    Lt,
}

#[derive(Clone, Copy)]
pub enum Cmd {
    Set { key: KeyId, val: Str, cond: SetCond, ttl: TtlArg, get: bool },
    Get { key: KeyId },
    Del { key: KeyId },
    Exists { key: KeyId },
    Type { key: KeyId },
    Expire { key: KeyId, at: Ms, cond: ExpireCond },
    Persist { key: KeyId },
    Pttl { key: KeyId },
    Keys,
    DbSize,
    Watch { client: ClientId, key: KeyId },
    Unwatch { client: ClientId },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Ok,
    Nil,
    Int(i64),
    Str(Str),
    /// Set of visible key ids, as a bitmask over 0..K. `KEYS` ordering is unspecified in
    /// Redis, so the model exposes a set rather than inventing an order.
    KeySet([bool; K]),
    Type(VType),
    NoType,
    Error,
}

// --------------------------------------------------------------- helpers

/// `setExpire` (db.c). Only ever called on a present key.
fn set_expire(w: &mut World, k: KeyId, at: Ms) {
    let k = kidx(k);
    w.slots[k].expire_at = at;
}

fn remove_expire(w: &mut World, k: KeyId) {
    let k = kidx(k);
    w.slots[k].expire_at = NO_EXPIRE;
}

/// `checkAlreadyExpired` (expire.c) -- an absolute time already in the past, on a master
/// that is not loading.
fn check_already_expired(w: &World, at: Ms) -> bool {
    at <= w.clock && w.role == Role::Master
}

// --------------------------------------------------------------- execution

/// Execute one command. This is ONE EXECUTION UNIT (server.c:1420-1432) -- the real
/// atomic unit in Redis, not "one command" in general: a whole EXEC or a whole script is
/// also one unit. Propagation is flushed by the caller at nesting 0.
pub fn exec_cmd(w: &mut World, c: ClientId, cmd: Cmd) -> Reply {
    let c = cidx(c);
    match cmd {
        Cmd::Set { key, val, cond, ttl, get } => cmd_set(w, key, val, cond, ttl, get),
        Cmd::Get { key } => match lookup_key_read(w, key) {
            Some(Value::Str(s)) => Reply::Str(s),
            None => Reply::Nil,
        },
        Cmd::Del { key } => cmd_del(w, key),
        Cmd::Exists { key } => {
            // EXISTS goes through lookupKeyRead, so on a master it DELETES a logically
            // expired key as a side effect. Measured: EXISTS -> 0 and DBSIZE 1 -> 0.
            match lookup_key_read(w, key) {
                Some(_) => Reply::Int(1),
                None => Reply::Int(0),
            }
        }
        Cmd::Type { key } => match lookup_key_read(w, key) {
            Some(v) => Reply::Type(v.vtype()),
            None => Reply::NoType,
        },
        Cmd::Expire { key, at, cond } => cmd_expire(w, key, at, cond),
        Cmd::Persist { key } => cmd_persist(w, key),
        Cmd::Pttl { key } => match lookup_key_read(w, key) {
            None => Reply::Int(-2), // no such key
            Some(_) => {
                let key = kidx(key);
                if w.slots[key].has_ttl() {
                    Reply::Int(w.slots[key].expire_at - w.clock)
                } else {
                    Reply::Int(-1) // exists, no TTL
                }
            }
        },
        Cmd::Keys => cmd_keys(w),
        Cmd::DbSize => cmd_dbsize(w),
        Cmd::Watch { client, key } => {
            watch::watch_key(w, client, key);
            Reply::Ok
        }
        Cmd::Unwatch { client } => {
            watch::unwatch_all(w, client);
            Reply::Ok
        }
    }
    // `c` is unused for now; it becomes load-bearing when MULTI/EXEC lands.
    .tap_client(c)
}

trait Tap {
    fn tap_client(self, _c: ClientId) -> Self;
}
impl Tap for Reply {
    #[inline(always)]
    fn tap_client(self, _c: ClientId) -> Self {
        self
    }
}

/// `setGenericCommand` (t_string.c:161-230).
fn cmd_set(w: &mut World, k: KeyId, val: Str, cond: SetCond, ttl: TtlArg, get: bool) -> Reply {
    let k = kidx(k);
    // GET reads the old value BEFORE the write, via the read path.
    let old = if get { lookup_key_read(w, k) } else { None };

    // NX/XX are evaluated against the LOGICAL existence of the key, i.e. after expiry.
    let exists = lookup_key_write(w, k).is_some();
    let allowed = match cond {
        SetCond::Always => true,
        SetCond::Nx => !exists,
        SetCond::Xx => exists,
    };
    if !allowed {
        return if get {
            match old {
                Some(Value::Str(s)) => Reply::Str(s),
                None => Reply::Nil,
            }
        } else {
            Reply::Nil // aborted SET replies nil
        };
    }

    // t_string.c:161-175. If the SET carries an ALREADY-ELAPSED absolute expire, the value
    // is never written: an existing key is DELETED and the command propagates as DEL (not
    // SET), while the client still gets +OK. Ordered AFTER the NX/XX check, before the
    // write.
    //
    // Found by differential testing against redis-server 8.9.241: the model previously
    // stored the value with a past expire, leaving a physically-present key. The server
    // leaves nothing. 102/400 schedules diverged on this.
    if let TtlArg::PxAt(at) = ttl {
        if check_already_expired(w, at) {
            if exists || w.slots[k].present {
                w.slots[k] = Slot::absent();
                w.dirty += 1;
                watch::key_modified(w, k);
                w.repl.push(Effect::Del { key: k });
            }
            return if get {
                match old {
                    Some(Value::Str(s)) => Reply::Str(s),
                    None => Reply::Nil,
                }
            } else {
                Reply::Ok
            };
        }
    }

    let keep = matches!(ttl, TtlArg::Keep);
    let prev_expire = w.slots[k].expire_at;

    w.slots[k].present = true;
    w.slots[k].value = Value::Str(val);
    w.slots[k].expire_at = match ttl {
        TtlArg::None => NO_EXPIRE, // plain SET clears the TTL
        TtlArg::Keep => {
            if exists {
                prev_expire
            } else {
                NO_EXPIRE
            }
        }
        TtlArg::PxAt(at) => at,
    };
    let _ = keep;

    w.dirty += 1;
    watch::key_modified(w, k);
    // Redis rewrites a relative TTL to an ABSOLUTE PXAT before propagating, so replaying
    // the log against a different clock reproduces the same TTL (t_string.c:178-195).
    // That is exactly what makes the replication-refinement property non-vacuous.
    w.repl.push(Effect::Set { key: k, val, pxat: w.slots[k].expire_at });

    if get {
        match old {
            Some(Value::Str(s)) => Reply::Str(s),
            None => Reply::Nil,
        }
    } else {
        Reply::Ok
    }
}

/// `delGenericCommand` (db.c:1442).
///
/// NOTE the master/replica divergence: on a master a logically expired key is deleted by
/// the lookup and DEL reports 0; on a WRITABLE replica the same call reports 1, because
/// `expireIfNeeded` never returns KEY_DELETED there. Out of scope while the fence pins a
/// read-only replica, but recorded so nobody "fixes" it later.
fn cmd_del(w: &mut World, k: KeyId) -> Reply {
    let k = kidx(k);
    if lookup_key_write(w, k).is_none() {
        return Reply::Int(0);
    }
    w.slots[k] = Slot::absent();
    w.dirty += 1;
    watch::key_modified(w, k);
    w.repl.push(Effect::Del { key: k });
    Reply::Int(1)
}

/// `expireGenericCommand` (expire.c:734-830), with the exact NX/XX/GT/LT semantics.
fn cmd_expire(w: &mut World, k: KeyId, at: Ms, cond: ExpireCond) -> Reply {
    let k = kidx(k);
    // expire.c:760 -- lookupKeyWrite. A logically expired key is deleted here and the
    // command then reports 0.
    if lookup_key_write(w, k).is_none() {
        return Reply::Int(0);
    }
    let current = w.slots[k].expire_at;

    match cond {
        ExpireCond::None => {}
        // expire.c:772 -- NX fails if any TTL is set.
        ExpireCond::Nx => {
            if current != NO_EXPIRE {
                return Reply::Int(0);
            }
        }
        // expire.c:780 -- XX fails if no TTL is set.
        ExpireCond::Xx => {
            if current == NO_EXPIRE {
                return Reply::Int(0);
            }
        }
        // expire.c:789 -- a persistent key counts as INFINITE, so GT always fails on it.
        ExpireCond::Gt => {
            if current == NO_EXPIRE || at <= current {
                return Reply::Int(0);
            }
        }
        // expire.c:800 -- a persistent key counts as infinite, so LT always SUCCEEDS on it.
        ExpireCond::Lt => {
            if current != NO_EXPIRE && at >= current {
                return Reply::Int(0);
            }
        }
    }

    // expire.c:809 -- an absolute time already in the past deletes the key and replies 1.
    // Contrast SET, which REJECTS a non-positive TTL with an error (t_string.c:250-255).
    // A single "setTTL" abstraction across SET and EXPIRE would be wrong.
    if check_already_expired(w, at) {
        w.slots[k] = Slot::absent();
        w.dirty += 1;
        watch::key_modified(w, k);
        w.repl.push(Effect::Del { key: k });
        return Reply::Int(1);
    }

    set_expire(w, k, at);
    w.dirty += 1;
    watch::key_modified(w, k);
    w.repl.push(Effect::PExpireAt { key: k, at });
    Reply::Int(1)
}

fn cmd_persist(w: &mut World, k: KeyId) -> Reply {
    let k = kidx(k);
    if lookup_key_write(w, k).is_none() {
        return Reply::Int(0);
    }
    if !w.slots[k].has_ttl() {
        return Reply::Int(0);
    }
    remove_expire(w, k);
    w.dirty += 1;
    watch::key_modified(w, k);
    w.repl.push(Effect::Persist { key: k });
    Reply::Int(1)
}

/// `keysCommand` (db.c:1638-1645).
///
/// KEYS filters with `keyIsExpired` and does NOT delete. Measured: after an elapsed PX,
/// `KEYS *` -> [] while DBSIZE STAYS 1.
fn cmd_keys(w: &mut World) -> Reply {
    let mut out = [false; K];
    let mut i = 0;
    while i < K {
        if w.slots[i].present && !key_is_expired(w, i) {
            out[i] = true;
        }
        i += 1;
    }
    Reply::KeySet(out)
}

/// `dbSize` (db.c:3148-3173) -- `kvstoreSize(db->keys)` with NO expiry filter whatsoever.
/// This is the single most important disagreement in the model: DBSIZE counts PHYSICAL
/// presence. Measured: 1 for a logically expired key, and it stays 1 after KEYS.
fn cmd_dbsize(w: &World) -> Reply {
    let mut n = 0i64;
    let mut i = 0;
    while i < K {
        if w.slots[i].present {
            n += 1;
        }
        i += 1;
    }
    Reply::Int(n)
}

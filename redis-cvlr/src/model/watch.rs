//! WATCH / CAS invalidation -- a faithful transcription of `touchWatchedKey`.
//!
//! Transcribed from redis/src/multi.c:387-426 as of 8.9.241.
//!
//! Two things make this worth specifying rather than assuming:
//!
//! 1. WATCH is VALUE-BLIND. `touchWatchedKey` never inspects the value -- it takes only
//!    the key name. Writing a key back to its original value still invalidates. A model
//!    that implements CAS as value comparison is wrong, and wrong in the direction that
//!    silently passes.
//!
//! 2. The `wk->expired` exemption is subtle enough that a model author will get it wrong,
//!    and it contains a control-flow oddity -- see `SUSPECTED DEFECT` below.

use super::state::*;

/// `watchCommand` -> `watchForKey` (multi.c:~320-340).
///
/// `wk->expired` records whether the key was ALREADY logically expired at WATCH time
/// (multi.c:330). Watchers are appended with `listAddNodeTail` (multi.c:332).
pub fn watch_key(w: &mut World, c: ClientId, k: KeyId) {
    if w.clients[c].watching[k] {
        return; // already watching -- multi.c checks this first
    }
    let expired_now = w.slots[k].present && super::expire::key_is_expired(w, k);
    w.clients[c].watching[k] = true;
    w.clients[c].watched_expired[k] = expired_now;
    w.watched.add_tail(k, c);
}

/// `unwatchAllKeys` (multi.c).
pub fn unwatch_all(w: &mut World, c: ClientId) {
    let mut k = 0;
    while k < K {
        if w.clients[c].watching[k] {
            w.clients[c].watching[k] = false;
            w.clients[c].watched_expired[k] = false;
            w.watched.remove(k, c);
        }
        k += 1;
    }
}

/// `touchWatchedKey` -- multi.c:387-426, transcribed BUG-FOR-BUG.
///
/// ```text
/// while ((ln = listNext(&li))) {
///     watchedKey *wk = ...; client *c = wk->client;
///     if (wk->expired) {
///         if (db == wk->db && equalStringObjects(key, wk->key) && dbFind(db, key->ptr) == NULL) {
///             wk->expired = 0;
///             goto skip_client;
///         }
///         break;                      // <-- multi.c:415
///     }
///     c->flags |= CLIENT_DIRTY_CAS;
///     unwatchAllKeys(c);
/// skip_client:
///     continue;
/// }
/// ```
///
/// SUSPECTED DEFECT (multi.c:415). All entries in this list watch THIS key in THIS db, so
/// `db == wk->db` and `equalStringObjects(key, wk->key)` both hold; the guard reduces to
/// "the key is now physically absent". Therefore, for a watcher that saw the key already
/// logically expired at WATCH time, if the key is now PRESENT (someone overwrote it), the
/// code `break`s out of the ENTIRE watcher list rather than `continue`ing. Consequences:
///   (a) that watcher is not dirtied, although the value it observed changed from
///       "logically absent" to a concrete value; and
///   (b) every watcher AFTER it in insertion order is silently skipped too, regardless of
///       its own `expired` flag.
///
/// (b) is the part that looks indefensible: those watchers have nothing to do with the
/// expired-at-WATCH case. Control flow and list ordering are confirmed against the source;
/// the end-to-end client-visible scenario has NOT been constructed. Treated here as a
/// candidate, not a finding. `multi_watch_touch_dirties_all_watchers` in
/// `specs/multi_rules.rs` is the rule that targets it -- it is EXPECTED TO FAIL, and if it
/// does, `drivers/difftest` must confirm the same behaviour on the real server before
/// anyone calls it a bug.
pub fn touch_watched_key(w: &mut World, k: KeyId, _from_expiry: bool) {
    // `listRewind`/`listNext` advance the iterator BEFORE running the body, so
    // `unwatchAllKeys(c)` removing the current node cannot disturb iteration (a client has
    // at most one entry per key). Snapshotting the order models that exactly.
    let order = w.watched.order[k];
    let n = w.watched.len[k];

    let mut i = 0;
    while i < n {
        let c = match order[i] {
            Some(c) => c,
            None => {
                i += 1;
                continue;
            }
        };

        if w.clients[c].watched_expired[k] {
            // multi.c:407-413 -- the already-expired key is now gone, so logically no
            // change: clear the flag and skip ONLY this client (`goto skip_client`).
            if !w.slots[k].present {
                w.clients[c].watched_expired[k] = false;
                i += 1;
                continue;
            }
            // multi.c:415 -- `break`. Abandons the ENTIRE remaining list.
            // See SUSPECTED DEFECT above. Transcribed as written, deliberately.
            return;
        }

        w.clients[c].dirty_cas = true;
        unwatch_all(w, c); // multi.c:419
        i += 1;
    }
}

/// `keyModified` -- every write path calls this. Kept separate from `touch_watched_key`
/// so the call sites read like the C.
pub fn key_modified(w: &mut World, k: KeyId) {
    touch_watched_key(w, k, false);
}

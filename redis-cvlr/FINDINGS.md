# Findings

Status vocabulary: `CANDIDATE` (code reading only) · `NOT REPRODUCED` (attempted, failed,
mechanism understood) · `CONFIRMED` (client-visible repro against the real server) ·
`CLOSED`.

---

## F-01 — `touchWatchedKey` abandons the watcher list instead of skipping one watcher

**Status: NOT REPRODUCED.** Anomaly in the source is real; no client-visible consequence
has been demonstrated. Do not report this as a bug.

### The code

`redis/src/multi.c:405-416`, inside the per-watcher loop of `touchWatchedKey`:

```c
if (wk->expired) {
    /* The key was already expired when WATCH was called. */
    if (db == wk->db &&
        equalStringObjects(key, wk->key) &&
        dbFind(db, key->ptr) == NULL)
    {
        /* Already expired key is deleted, so logically no change. Clear
         * the flag. Deleted keys are not flagged as expired. */
        wk->expired = 0;
        goto skip_client;
    }
    break;                      /* <-- multi.c:415 */
}
```

Every entry in `clients` watches this key in this db (`clients = dictFetchValue(
db->watched_keys, key)`), so `db == wk->db` and `equalStringObjects(...)` both hold and the
guard reduces to *"the key is now physically absent"*. Therefore, when a watcher has
`wk->expired == 1` and the key is **present**, control leaves the **entire** loop.

Two consequences, of which only the second looks indefensible:

1. That watcher is not dirtied, although what it observed changed from *logically absent*
   to a concrete value.
2. Every watcher **after** it in insertion order (`listAddNodeTail`, multi.c:332) is
   skipped too — including watchers with `wk->expired == 0`, which have nothing to do with
   the expired-at-WATCH case.

### Why it is anomalous

The sibling function `touchAllWatchedKeysInDb` (multi.c:440-486), which handles the
FLUSHDB / SWAPDB path, has the structurally identical branch and uses **`continue`** in
both arms (multi.c:467 and :472), never `break`. Same author, same file, same situation,
opposite control flow.

### Attempted reproduction — failed

Against `redis-server 8.9.241` with `--enable-debug-command yes` and
`DEBUG SET-ACTIVE-EXPIRE 0`:

1. `SET k v1 PX 50`, sleep 250 ms → `k` is logically expired, still physically present
   (`DBSIZE` = 1).
2. Client **A**: `WATCH k` → `wk->expired = 1` (multi.c:331).
3. Client **C**: `SET k v2` → `k` present again.
4. Client **B**: `WATCH k` → `wk->expired = 0`, appended **after** A.
5. Client **C**: `SET k v3`.
6. Client **B**: `MULTI; GET k; EXEC`.

Predicted if the bug bites: B's `EXEC` succeeds. **Observed: `EXEC` returned nil — B was
correctly invalidated.** A control run without watcher A also correctly aborted.

### Why it did not reproduce

Step 3 defeats it. `SET` goes through `lookupKeyWrite` → `expireIfNeeded`, which
**deletes** the logically-expired key and calls `signalModifiedKey` → `touchWatchedKey`
*while the key is absent*. That pass takes the `dbFind(...) == NULL` arm and clears
`wk->expired = 0` for A (multi.c:413), taking `goto skip_client` rather than `break`. Only
then does the write make the key present. By the time B watches, no watcher has
`expired == 1` left, so the `break` is never reached.

Generalizing: any write path that first lazy-expires the key clears the flag before the key
becomes present, so the `break` is unreachable from it.

### Route 2 attempted: SWAPDB — also failed

`f01_swapdb_reachability_probe` in `tests/difftest.rs`. Setup: db1 holds a key that is
logically expired but physically present (`DBSIZE` = 1 confirms); client A watches that
name in db0 where it does not exist; `SWAPDB 0 1` brings the expired-but-present key into
db0, which should make `touchAllWatchedKeysInDb` take the
`!exists_in_emptied && keyIsExpired(replaced_with)` arm and set A's `wk->expired = 1`
*with the key present* (multi.c:472-474); client B then watches it; the key is overwritten.

**Observed: both A's and B's `EXEC` returned nil — both correctly invalidated.** The
overwriting `SET` still lazy-expires first, which clears the flags before the key becomes
present, exactly as in route 1.

Two independent routes now tried, both defeated by the same mechanism.

### The remaining open route

`touchAllWatchedKeysInDb` can **set** `wk->expired = 1` while the key is *present*
(multi.c:472-474):

```c
} else if (!exists_in_emptied && keyIsExpired(replaced_with, key->ptr, NULL)) {
    /* Non-existing key is replaced with an expired key. */
    wk->expired = 1;
```

That is the **SWAPDB** path: a client watching a key that does not exist in db *N*, after
`SWAPDB` brings in a logically-expired-but-present key of the same name, ends up with
`expired = 1` **and** the key physically present — the exact precondition the `break`
needs, reached without a deletion.

The obvious follow-up (`SWAPDB`, then a write) still runs into the lazy-expire clear. What
is needed is a path that reaches `touchWatchedKey` with the key present and a stale
`expired` flag — candidates not yet tried: `RESTORE ... REPLACE`, `RENAME` onto the key,
`MOVE`/`COPY` into the db, replica application of a master write, and a second `SWAPDB`.

### Concrete execution of the rule (2026-09-19)

`multi_touch_dirties_every_watcher` fails on the model for every seed tried (20 000 runs,
0 rejected). That is the designed outcome, but read it precisely:

The rule reaches the precondition by **direct state mutation** -- it sets `present = true`
and `expire_at = NO_EXPIRE` on the slot rather than issuing a command. So what it
establishes is a CONDITIONAL:

> *If* a watcher can hold `watched_expired == true` while the key is physically present,
> *then* a watcher later in the list is silently skipped.

The consequent is proved. The **antecedent's reachability is the open question**, and the
failed server repro above is evidence against it: every ordinary write path lazy-expires
first, which clears the flag. So the honest reading is "the `break` is a live hazard
guarded only by an incidental ordering property elsewhere in the code", not "Redis has a
WATCH bug".

This is exactly the failure mode a bounded model has: it can prove an implication while
saying nothing about whether the hypothesis is reachable. Anyone extending this should
either (a) find a command sequence that establishes the antecedent -- `SWAPDB` is the
candidate -- or (b) add a reachability rule that quantifies over command sequences only,
and show the antecedent is unreachable, which would justify closing F-01 as CLOSED-BENIGN.

### What to do with it

- `model/watch.rs` transcribes the `break` **as written**, deliberately.
- `specs/multi_rules.rs` carries the rule that targets it. It is expected to FAIL against
  the model. A model failure is **not** a Redis bug — it localizes a discrepancy that
  `drivers/difftest` must then confirm against the real server.
- If someone does find a client-visible repro, this is a WATCH/EXEC soundness bug and the
  best possible argument for this project. If instead it is proved unreachable, that is
  worth writing down too, and the `break` should still be reported upstream as a latent
  hazard given its sibling uses `continue`.

---

## What differential testing has caught

The harness in `tests/difftest.rs` is the only evidence in this project that touches the
shipped C. Its track record so far, recorded because a harness that has never caught
anything is not known to work:

**D-01 — `SET k v PXAT <elapsed>` writes nothing and deletes.** The model stored the value
with a past expire, leaving a physically-present, logically-expired key. The server leaves
**no key at all**: `t_string.c:161-175` takes an early-return branch that deletes an
existing key, propagates `DEL` (not `SET`), replies `+OK`, and never writes the value. The
branch sits *after* the NX/XX and IFEQ checks.

*Caught by*: 102 of 400 schedules diverged, all in the same direction (`model dbsize 1 vs
server 0`). *Consequence*: property **P-06 was stated wrongly** — "`SET ... PXAT t`
installs exactly `t`" is false for elapsed `t`. Both the model and the property were
corrected. This is the case for differential testing in one example: the error was in the
*specification*, so no amount of proving the model would have surfaced it.

**Negative results are results.** F-01 has now survived two reproduction attempts. That is
recorded above rather than quietly dropped.

---

## F-02 — deferred surface, recorded so it is not lost

See `../CLAUDE.md` § Deferred. The 8.9.241-only command surface (`DELEX`, `INCREX`,
`MSETEX`, `HSETEX`/`HGETEX`/`HGETDEL`, `HIMPORT`, `SET IFEQ`/`IFDEQ`, `BLESS`, `GCRA`,
`HOTKEYS`, the `AR*` family and `OBJ_ARRAY`, `LMOVEM`/`BLMOVEM`, `SUNIONCARD`/`SDIFFCARD`,
`BACKUP`, `XREAD MAXCOUNT`/`MAXSIZE`, and the `LISTPACK_EX`/`TMPL_*`/`SLICED_ARRAY`
encodings and `*-lrm` policies) is out of scope for iteration 1 by decision, not because it
is unimportant. Most of it has no public documentation, so the C and the tcl suite are the
only sources.

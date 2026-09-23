# Redis user-visible behavior — property catalog

Iteration 1: **keyspace + expiry**, standalone.

## Read this first

**What is proven here is proven about a MODEL, not about Redis.** CVLR is a Rust library
whose verification vocabulary is a set of `extern "C"` `CVT_*` symbols the Certora Prover
interprets symbolically; the Prover ingests a `wasm32-unknown-unknown` module built by
cargo. Redis is C, and there is no C frontend. So `src/model/` is a hand-written abstract
state machine that *transcribes* cited Redis C, and every rule is a statement about that
transcription.

The correspondence argument is therefore not the Prover's job. It rests on three legs,
honestly ranked:

1. **Differential execution** against the real `redis-server` 8.9.241 (already built in
   this workspace). `src/driver/concrete.rs` implements the `CVT_*` ABI so the *same* rule
   bodies execute over a seeded stream; the schedules they draw are replayed over RESP and
   compared. This is sampling, not proof — but it is the only leg that touches the
   shipped code.
2. **Line-level traceability.** Every model function cites the `redis/src` range it
   transcribes. A reviewer who knows Redis can check the transcription without reading
   Rust.
3. **Redis's own oracles**: `assert_replication_stream`
   (tests/test_helper.tcl:787-864), `debug_digest`, `csvdump` (tests/support/util.tcl).

Most of this catalog has not been through the Prover; see § Prover runs for what has, and
read that section's caveats before citing any of it. A rule that compiles is not a verified
rule.

## Scope fence

Fixed for iteration 1, and load-bearing: a property is only as meaningful as its scope.

| knob | pinned to |
|---|---|
| topology | standalone; `cluster_enabled = 0` |
| databases | 1 |
| `maxmemory` | 0 — eviction excluded entirely |
| persistence | `appendonly no`, `save ''` |
| types | STRING only |
| keys | `K = 3` opaque ids |
| clients | `C = 2` |
| value length | ≤ 8 bytes |
| floats | none — CVLR has no `f32`/`f64` at all |
| out of scope | modules, Lua/FUNCTION, ACL, cluster, replication topology, the 8.9.241-only surface (see `FINDINGS.md` F-02) |

`optimistic_loop` + `loop_iter` in the conf **assume away** every execution exceeding the
bound. "For all keys" means "for all 3 keys". Never report a bounded result as unbounded.

## Status vocabulary

`unproven` · `prover-pass` · `prover-fail` · `expected-fail` · `NOT-EXPRESSIBLE` · `deferred`

A property may additionally be marked **differentially tested**, meaning the model's
behavior for it has been compared against a real `redis-server` 8.9.241. That is sampling,
not proof, and it is independent of the Prover status.

## Prover runs

**Early work. Treat everything here as provisional.** These are the first runs through
Certora Sunbeam (2026-09-22), made while the toolchain settings were still being worked
out, and a majority of the verdicts are not yet trusted. The configuration itself changed
several times during the day; the values recorded below are the ones that produced these
particular results, not a settled setup. The two confs also need different `Cargo.toml`
profiles -- see the comment above `opt-level`.

### `confs/keyspace_expiry.conf` -- the catalog

https://prover.certora.com/output/33158/9f0d60c87df34e63bcca1df9e2252cfe?anonymousKey=4a12adf1f3f3895dfed0d5956aa803a6cb3511c7

`opt-level = 3`, `loop_iter 8`, `optimistic_loop false`, `maxBlockCount 200000`, no
`multi_assert_check`. Zero exceptions, zero unwinding asserts, zero block-count failures.

**Verified**, reproduced in two independent runs: **P-04**, **P-11**, **P-12**.

**Reported Violated, NOT yet believed**: P-01, P-01b, P-07, P-09, P-10, P-10b. Every rule
that passes is one that does not call `draw_key()`; every rule that fails does. Against
these verdicts: exhaustive enumeration passes 192/192, the concrete driver passes
20 000/20 000 per rule, difftest agrees with a real redis-server on 3000/3000 schedules,
and P-01 verified 8/8 when instantiated at a constant key. A per-assert triage run also
returned a self-contradictory result for P-09 (`assert_1` proved
`repl.get(before) == Some(Del)` while `assert_2` claims it may be `Some(Multi)`).
Unexplained. Do not mark these `prover-fail` until it is.

**No verdict**: P-02, P-06 (UNKNOWN), P-03, P-05 (TIMEOUT).

### `confs/findings.conf` -- P-08 / F-01

https://prover.certora.com/output/33158/726cd72fb6b3474f99dcf549917f83bc?anonymousKey=2e8361d66512bcaf94d35a90df2a56c83bd52faa

`opt-level = "z"`, `loop_iter 8`, `optimistic_loop false`, `multi_assert_check true`.

**P-08: 3/3 asserts Violated with `ProverInternalChecks` Verified** -- the loop-unwinding
condition was proved, not assumed. That is the `expected-fail` outcome the entry predicts,
now over every state in the bound rather than the 20 000 sampled by the concrete driver. It
does not change F-01's status: see `FINDINGS.md`.

Note the asymmetry: P-08 is the only rule here with a Violated verdict worth trusting, and
it is trustworthy precisely because `multi_assert_check` was on, so `ProverInternalChecks`
reported as its own row. The six catalog Violateds above have no such row.

## Form vocabulary

`integrity` (post vs pre for one operation) · `invariant` (one rule per invariant ×
mutating operation) · `interleaving` (bounded schedule over distinct clients) · `panic` /
`non-panic` · `satisfy` (reachability)

---

## Keyspace and expiry

### P-01 — String-Set-Integrity
**Form** integrity · **Rule** `string_set_integrity` · **Status** unproven

After an unconditional `SET k v` with no TTL argument: `k` is present with value `v`, `k`
carries no TTL, and no other key changes physically.

*Evidence* `t_string.c` `setGenericCommand`.

### P-01b — Set-NX-XX-Uses-Logical-Existence
**Form** integrity · **Rule** `string_set_nx_xx_uses_logical_existence` · **Status** unproven

`SET NX` succeeds iff the key is *logically* absent — i.e. after expiry is applied, not
merely physically absent. `SET XX` is the complement. A key that is physically present but
past its TTL counts as absent for NX.

### P-02 — Expiry-Visibility-Split ★
**Form** integrity · **Rule** `expire_visibility_split` · **Status** unproven

The folklore statement *"a key past its TTL is never observed"* is **false**. The true
statement names the path:

> No command that reaches its key through `lookupKey*()` returns the value of a key whose
> expire < `commandTimeSnapshot()`.

Everything else disagrees, and the disagreements are client-visible. Measured on
`redis-server 8.9.241` for one key with an elapsed `PX`:

| observation | result | deletes? |
|---|---|---|
| `DBSIZE` | 1 | no — no expiry filter at all (db.c:3148-3173) |
| `INFO keyspace` | `db0:keys=1,expires=1` | no (server.c:7272-7283) |
| `KEYS *` | `[]` | **no** — filters, leaves the key (db.c:1638-1645) |
| `EXISTS k` | 0 | **yes** — DBSIZE then drops to 0 |
| `HLEN h` | 2 | no — counts expired fields; `HGETALL` returns 1 (t_hash.c:5596-5603) |

This is why the model's keyspace is **physical** and visibility is a derived function. A
model with one `HashMap<Key, Value>` cannot state P-02 at all.

### P-02b — Expiry-Is-Seven-Way-Conditional
**Form** integrity · **Status** unproven (partially covered by P-10, P-10b)

`expireIfNeeded` (db.c:3004-3073) returns **without deleting** via at least seven paths,
several of which fire on a plain standalone master: `asmIsKeyInTrimJob`;
`EXPIRE_ALLOW_ACCESS_EXPIRED`; `server.loading || server.allow_access_expired`; the
replica/cluster branch — guarded by `masterhost != NULL || server.cluster_enabled`, so
**enabling cluster alone changes expiry on a master**; `!confAllowsExpireDel()`;
`EXPIRE_AVOID_DELETE_EXPIRED`; and `PAUSE_ACTION_EXPIRE`.

Any `exists` predicate must be parameterized by (role, client flags, loading, pause state,
`cluster_enabled`, config). Most informal accounts list two of these.

### P-03 — Lazy-vs-Active-Off-By-One
**Form** integrity · **Rule** `expire_lazy_active_boundary` · **Status** unproven

At exactly `clock == expire_at`, a lazy lookup sees the key **valid** (`now > when`,
strict, db.c:2954) while the active cycle **deletes** it (`now >= when`, expire.c:40-41).
Hash-field TTL uses a third comparison. A model that picks one comparison disagrees with
the others at the boundary.

### P-04 — Expire-Condition-Semantics
**Form** integrity · **Rule** `expire_condition_semantics` · **Status** unproven

`EXPIRE` `NX`/`XX`/`GT`/`LT` return 1 and install the TTL exactly when their condition
holds, and 0 with the TTL untouched otherwise. A key with **no** TTL counts as
+infinity — so `GT` can never beat it and `LT` always does (expire.c:789, :800).

Note also that `SET` rejects a non-positive TTL with an error (t_string.c:250-255) while
`EXPIRE` accepts a past time, deletes the key and replies 1 (expire.c:809). **A single
`setTTL` abstraction across the two would be wrong.**

### P-05 — Expires-Subset-Of-Keys
**Form** invariant · **Rule** `keyspace_expires_subset_of_keys` · **Status** unproven

No absent slot carries a TTL. In 8.9.241 this is *structural* rather than an invariant to
prove: the value is a `kvobj` embedding both key and expire, and `db->keys`/`db->expires`
are kvstores over the same pointers (object.h:31-66). The rule pins that the **model**
preserves it under every operation.

### P-06 — Set-TTL-Argument-Semantics
**Form** integrity · **Rule** `string_set_ttl_argument` · **Status** unproven ·
**differentially tested**

`SET ... KEEPTTL` preserves an existing TTL; plain `SET` clears it; `SET ... PXAT t`
installs exactly `t` **when `t` is in the future**.

An **already-elapsed** `t` is a different operation entirely: the value is never written,
any existing key is deleted, the command propagates as `DEL` rather than `SET`, and the
client still gets `+OK` (t_string.c:161-175, ordered after the NX/XX check).

> This property was originally stated without that clause, i.e. **wrongly**. It was caught
> by differential testing (FINDINGS.md D-01), not by reading the C — 102 of 400 schedules
> diverged. Proving the model would never have found it, because the error was in the
> specification.

### P-09 — Lazy-Expiry-Propagates-A-Bare-DEL
**Form** integrity · **Rule** `expire_lazy_propagates_bare_del` · **Status** unproven

A read that lazily expires a key propagates exactly one `DEL`, **not** wrapped in
MULTI/EXEC. *Oracle* tests/unit/expire.tcl:809 and :830 pin this; contrast
tests/unit/multi.tcl:398 vs :412 for the wrapped case.

### P-10 — Replica-Hides-But-Does-Not-Delete
**Form** integrity · **Rule** `expire_replica_hides_without_deleting` · **Status** unproven

On a read-only replica a logically expired key is invisible to a normal client but remains
**physically present** — expiry there is driven by `DEL`s from the master (db.c:3042-3045).
A genuine master/replica observable divergence: `DBSIZE` disagrees between them for the
same logical dataset.

### P-10b — Master-Link-Client-Never-Sees-Expiry
**Form** integrity · **Rule** `expire_master_link_sees_expired_key_as_valid` · **Status** unproven

A command arriving on the replication link (`CLIENT_MASTER`) treats a key past its TTL as
**valid** (db.c:3043). Expiry is a function of *who is asking*, not only of the state.

### P-11 — Active-Expiry-Only-Removes
**Form** invariant · **Rule** `keyspace_active_expiry_only_removes` · **Status** unproven

The active expire cycle never creates a key, never alters a surviving key's value or TTL,
and only removes keys whose TTL has elapsed. Modeled as an **adversarial step** the prover
may insert anywhere — which turns "a key may vanish at any moment" from prose into a
hypothesis every other rule must survive.

### P-12 — DBSIZE-Counts-Physical-Presence
**Form** integrity · **Rule** `keyspace_dbsize_counts_physical` · **Status** unproven

`DBSIZE` equals the count of physically present slots with no expiry filtering, and is
itself a pure observation.

---

## Concurrency

All four dimensions requested are catalogued. Two are expressible; two are not, and are
recorded with the reason rather than dropped.

CVLR supplies **nothing** for concurrency — no threads, locks, memory ordering,
happens-before or scheduler primitive anywhere in its ~15k lines. Where concurrency *is*
expressible, it is because the schedule has been **reified into the abstract state** and
quantified over with nondet.

### Dimension A — client-visible contracts · **expressible**

**Axiom** (not a theorem — stated as such): one `Step` is atomic. Justified by citation:
IO threads parse but refuse to execute and hand the client back (networking.c:3927-3934);
the real atomic unit is the *execution unit* (server.c:1420-1432), which also freezes the
clock, and propagation flushes only at nesting 0 (server.c:4060-4075). **Because this is
an axiom, no bug that violates it can be found here.**

#### P-07 — WATCH-Is-Value-Blind
**Form** interleaving · **Rule** `multi_watch_is_value_blind` · **Status** unproven

A write to a watched key dirties the watcher's CAS **even when the write restores the
original value**. CAS is key-name-based; `touchWatchedKey` never inspects the value
(multi.c:387-425). The rule deliberately *assumes* the value is unchanged — that is what
makes it state something a value-comparison CAS model gets wrong.

#### P-08 — Touch-Dirties-Every-Watcher
**Form** interleaving · **Rule** `multi_touch_dirties_every_watcher` · **Status** **expected-fail**

Targets `FINDINGS.md` **F-01**. `touchWatchedKey` `break`s out of the entire watcher list
at multi.c:415 rather than skipping one watcher; its sibling `touchAllWatchedKeysInDb`
uses `continue` in the identical branch. The model transcribes the `break` as written, so
this rule fails.

**A failure here is not a Redis bug.** The rule establishes a conditional; a client-visible
repro was attempted against the real server and **failed**. See F-01 for the full analysis
and the one open route (`SWAPDB`).

#### Not yet written (iteration 2)
- Execution-unit MULTI framing: a unit is wrapped in MULTI/EXEC iff it emitted >1 op and
  the direct command lacks `CMD_TOUCHES_ARBITRARY_KEYS` (server.c:4005-4019 — only `SCAN`
  and `RANDOMKEY` carry it).
- `EXEC`'s three distinct outcomes: `-EXECABORT` (queue-time error), null array (WATCH
  invalidated), normal array whose elements may be errors. Runtime errors do **not** abort.
- Blocked-client FIFO among type-matching waiters (blocked.c:620-677), with `block_seq` as
  a ghost ordering witness.
- Blocking degradation: inside MULTI/script, `BLPOP key 0` returns a null array instead of
  blocking (t_list.c:1364-1368) — one command, two contracts.
- EXEC's frozen clock: no TTL can expire between sub-commands.

### Dimension B — replication / durability · **partly expressible**

Safety over the effect log is expressible; **all liveness is not**.

- *Expressible*: replication refinement — replaying the emitted `ReplLog` onto a fresh
  world reproduces the producer's snapshot. Non-vacuous because Redis rewrites relative
  TTLs to absolute `PXAT` before propagating (t_string.c:178-195).
- `NOT-EXPRESSIBLE`: "an acknowledged write eventually reaches the replica", "master and
  replica digests eventually converge". Redis itself only claims convergence *after
  quiescence* (tests/integration/replication-psync.tcl).
- `NOT-EXPRESSIBLE`: durability. `appendfsync` is a property of the **event loop**, not of
  a command — the guarantee is the positional ordering of `flushAppendOnlyFile`
  (server.c:2070) before `handleClientsWithPendingWrites` (server.c:2117). A state model
  can reproduce offset arithmetic but cannot speak to fsync.
- Note `WAIT` counts replicas that *applied to memory*, not fsynced, and says nothing
  about which replicas are eligible for election.

### Dimension C — thread safety / data races · **NOT-EXPRESSIBLE**

Recorded as a requirement, with the reason and the tool that could discharge it.

CVLR has no atomic, lock, fence, happens-before or scheduler primitive. The two races
Redis itself documents are structurally out of reach:

1. `c->flags` `CLIENT_DIRTY_CAS` is written by the main thread while IO threads read other
   bits of the same word — suppressed with `REDIS_NO_SANITIZE("thread")` (multi.c:386-389).
2. Reply buffers can hold refcounted pointers into **live keyspace `robj`s**, which IO
   threads dereference at write time; async flush must `pauseAllIOThreads()` and deep-copy
   (lazyfree.c:215-232).

Everything in `src/tsan.sup` is likewise unreachable. Also note `atomicIncr`/`atomicGet`
are `memory_order_relaxed` (atomicvar.h:100-110), so INFO counters carry no cross-thread
ordering guarantee and cannot ground a happens-before claim.

**What would discharge this**: ThreadSanitizer (already wired into Redis's CI), or a
memory-model tool. Not this one. Stated plainly so nobody reads a green CVLR run as
"Redis is thread-safe".

### Dimension D — cluster / atomic slot migration · **deferred**

`cluster_asm.c` is ~3,900 lines introducing a fork+snapshot+stream+write-pause handoff, a
new `BLOCKED_POSTPONE_TRIM` state, wholesale WATCH invalidation over a slot range, an
indefinite write pause (`LLONG_MAX` nominal deadline, cluster_legacy.c:6697), and a new
user-visible error `-TRYAGAIN Slot is being trimmed` (cluster.c:1543). Paused clients are
postponed, not errored — a write can block silently for seconds, and expiry and eviction
freeze with it, so **TTLs appear to stretch**.

This is a larger concurrency surface than classic cluster and is its own project. Recorded
here so the omission is visible. Note the scope fence pins `cluster_enabled = 0`, which
also removes `KEY_TRIMMED` from `expireIfNeeded`.

---

## Excluded, with reasons

| area | why |
|---|---|
| eviction victim choice | explicitly approximate and randomized (redis.conf:1277-1279); `performEvictions()` runs before *every* command when `maxmemory != 0` and can delete across all DBs. Pinned out via `maxmemory=0`. |
| floats | CVLR has no `f32`/`f64` — no `Nondet`, no `CvlrLog`, no conversion. ZSET scores, `INCRBYFLOAT`, GEO, HLL. |
| encodings | not a function of the abstract state: quicklist→listpack uses **half** the threshold when shrinking (t_list.c:80-83), so identical contents can carry different encodings depending on history. |
| liveness (all) | every CVLR rule is a safety property over one bounded execution. |
| unbounded iteration | dict rehash, listpack scans, quicklist traversal. |
| debug surface | `DEBUG DIGEST`/`OBJECT`/`SET-ALLOW-ACCESS-EXPIRED`, `RANDOMKEY`'s give-up branch — deliberately abstraction-breaking. Excluded explicitly rather than modeled wrongly. |

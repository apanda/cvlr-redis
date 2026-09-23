# redis-verus — property catalog

Companion to `../redis-cvlr/PROPERTIES.md`. Numbering is `V-NN` to keep it distinct: these
are **not** the same statements as the `P-NN` entries, and where they overlap the scope
differs.

## Read this first

**What is proven here is proven about a MODEL, not about Redis.** `src/model/` is a hand
transcription of cited Redis C. Nothing in this directory executes Redis, and unlike
`../redis-cvlr` this leg is **not** currently wired into the differential harness, so its
agreement with the real server is *unverified* — see "Known gaps".

**Verus proves, it does not refute.** A failed proof means "could not prove", not "false":
the property may hold and the invariant be too weak. There are no counterexamples.
`--expand-errors` marks which conjunct of a failing predicate broke, which is usually
enough to localise.

**The model is smaller than Redis and smaller than the CVLR model.** Specifically it has
**no values**: `Slot` is `{ present: bool, expire_at: i64 }`. Every statement below is
about presence and TTL only. Nothing here says what a read returns.

## Scope

| knob | this leg |
|---|---|
| keyspace size | **unbounded** (`Vec<Slot>`, any length) |
| command sequence length | **unbounded** (bounded only by `i64` clock overflow) |
| commands modelled | `GET`, the active expire cycle, clock ticks. **No `SET`, no `DEL`, no `EXPIRE`** |
| values | **none** |
| watch table | **none** |
| topology | role and caller are fields; both master and replica reachable |
| replication log | present as a `Vec<Effect>`, append-only, unbounded |

Where CVLR pins `K = 3`, `C = 2`, `REPL_CAP = 8` and bounded step counts, this leg has no
such bounds. That is the single reason it exists.

## Status vocabulary

`verified` — Verus discharges it, and a non-vacuity witness exists where the statement is
a `requires`-guarded `exec fn`.
`verified (no witness)` — discharged, but nothing rules out unsatisfiable preconditions.
`not stated` — named here because it is worth proving, not because it is proved.

---

## V-01 — Well-formedness is preserved by every reachable state
**Status** verified · **Where** `model/step.rs::run`, invariant `state.rs::wf`

For any starting state satisfying `wf`, and **any sequence of steps of any length**, the
final state satisfies `wf`: no absent slot carries a TTL.

*Precisely:* quantified over all `Vec<Step>` whose elements are `step_ok`, all keyspace
sizes, all starting clocks below the overflow guard. Steps are drawn from `{Get,
ActiveExpire, ClockTick}` only.

*Relation to CVLR P-05:* P-05 assumes the invariant, runs **one** drawn command at `K = 3`,
and re-asserts it, with `optimistic_loop` assuming away executions past the unroll bound.
V-01 is the general statement. It is **weaker in one respect**: P-05's command alphabet
includes `SET`, `DEL`, `EXPIRE`, `PERSIST`, `KEYS`, `DBSIZE` and `WATCH`, and V-01's does
not. V-01 is not a strict strengthening of P-05.

## V-02 — No resurrection
**Status** verified · **Where** `model/step.rs::run` and `::step`

Over any sequence of steps, the set of physically present keys is monotonically
non-increasing: `final.slots[j].present ==> old.slots[j].present` for every `j`.

*Precisely:* true for the alphabet `{Get, ActiveExpire, ClockTick}`. This is a property
**of that alphabet**, not of Redis in general — `SET` obviously creates keys. It says that
no *read*, *clock tick* or *expire cycle* can bring a deleted key back. Adding `SET` to the
model will require restating it.

*Relation to CVLR:* not in `../redis-cvlr/PROPERTIES.md` at all. It cannot be phrased in a
bounded single-execution prover.

## V-03 — The active expire cycle only removes due keys
**Status** verified · **Where** `model/step.rs::active_expire_cycle`

For a keyspace of **any size**, one pass of the cycle:
(a) never makes an absent key present;
(b) leaves every surviving slot byte-identical;
(c) removes a key only if `active_cycle_would_expire` held of it in the pre-state, i.e.
    `expire_at >= 0 && clock >= expire_at` (expire.c:40-41, the non-strict comparison).
It also preserves `wf` and the whole configuration frame.

*Relation to CVLR P-11:* the same three clauses, but P-11 states them as 15 hand-written
assertions over exactly 3 slots. V-03 is a loop invariant over any number.

*Caveat:* this is one pass of the cycle over the whole keyspace. Redis's real cycle is
incremental and randomised (`activeExpireCycle`, expire.c) and visits a sample, not
everything. The model's cycle is an over-approximation of one full sweep.

## V-04 — Replica hides an expired key without deleting it
**Status** verified, with witness · **Where** `specs/expire_props.rs`

On a read-only replica, with a normal (non-master-link) caller, no debug escape, no pause,
cluster off: `GET` on a present-but-logically-expired key returns `Nil`, leaves the key
physically present, and appends nothing to the replication log.

*Relation to CVLR P-10:* the same three assertions, on the same executed command
(`cmd_get`), asserted about the post-state. Differences: unbounded `k`, and no value
comparison because the model has no values.

## V-05 — A master-link client sees an expired key as valid
**Status** verified, with witness · **Where** `specs/expire_props.rs`

Same state as V-04 but `caller == MasterLink`: `GET` returns non-`Nil`, the key stays
present, nothing is propagated. (db.c:3043.)

## V-06 — V-04 and V-05 are genuine opposites
**Status** verified · **Where** `specs/expire_props.rs::p10_and_p10b_are_opposites`

On the shared precondition, `expire_status(...) == Valid` **iff** `caller == MasterLink`.
Exists so that a specification error making both V-04 and V-05 trivially true would fail.

---

## Known gaps — read before citing any of the above

1. **No differential wiring.** Two transcriptions of the same C can drift and nothing
   currently detects it. The model is deliberately executable so it can be driven by
   `../redis-cvlr/tests/difftest.rs`; until that exists, agreement between the two legs is
   unverified.
2. **No values.** Nothing above constrains what a read returns.
3. **Three commands.** `SET`, `DEL`, `EXPIRE`, `PERSIST`, `KEYS`, `DBSIZE`, `WATCH` are all
   absent, so V-01 and V-02 quantify over a much smaller alphabet than their CVLR analogues.
4. **Non-vacuity is only partly checked.** V-04 and V-05 have executable witnesses. V-01,
   V-02, V-03 and V-06 do not; their preconditions are believed satisfiable but nothing
   proves it.
5. **The mutation battery is hand-chosen** (`just mutate`, 5 bugs, all caught) with
   knowledge of what the proofs assert. It is evidence the proofs are not vacuous, not a
   measured kill rate.

## Not stated, and worth stating

Replication refinement (replaying the effect log onto an empty database reproduces the
producer's visible state); relative-to-absolute TTL rewriting preserving the expiry
instant; expiry monotone in time; lazy expiry idempotent; physical count ≥ visible count
with the gap exactly the expired-but-present set; a total characterisation of
`expire_status` over (role, caller, pause, cluster, config); WATCH invalidation over an
unbounded watcher list, including the F-01 conditional.

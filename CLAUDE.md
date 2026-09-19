# CVLR specification for Redis — working notes

## What this workspace is

Goal: **a CVLR specification for Redis focused on behavior visible to users, including
concurrency and correctness requirements.**

| path | what it is | role |
|---|---|---|
| `cvlr/` | Certora Verification Language for Rust, v0.6.1, 12 crates | the spec language |
| `stellar-contracts/` | OpenZeppelin Soroban contracts carrying ~484 real CVLR rules | the style corpus |
| `redis/` | Redis Open Source **8.9.241** (an 8.10 pre-release), C | the subject |
| `redis-cvlr/` | the specification — `PROPERTIES.md`, `FINDINGS.md`, model, rules, drivers | the deliverable |

Checkouts are jujutsu (`jj`), not git. `stellar-contracts` working copy is exactly
`port@origin` with no local edits — that is the branch to read.

## The central structural fact

**The Certora Prover, on this stack, ingests a `wasm32-unknown-unknown` module built by
`cargo`. CVLR is a Rust-only library. Redis is C.** There is no C frontend anywhere in
`cvlr/`. Everything else follows from this.

CVLR's entire semantics is a set of `extern "C"` `CVT_*` symbols the Prover interprets
symbolically (`cvlr/cvlr-asserts/src/core.rs:1-12`). `#[rule]` only adds `#[no_mangle]`,
prepends `cvlr_rule_location!()`, appends `cvlr_vacuity_check!()`
(`cvlr/cvlr-macros/src/lib.rs:20-36`) — the Prover finds rules by exported symbol name.

Build path used by every working example:

```
just build                       # cargo +nightly-2024-11-22 build --target=wasm32-unknown-unknown
                                 #   --release --features certora   (RUSTFLAGS="-C strip=none")
  -> target/wasm32-unknown-unknown/release/<crate>.wasm
certora_build.py                 # emits {project_directory, sources, executables, success, ...}
confs/<name>.conf                # {"build_script": "../certora_build.py", "rule": [...], ...}
```

### Verified in-session (2026-09-19), do not re-derive

- A plain Rust crate depending on local `cvlr` 0.6.1, built with
  `RUSTFLAGS="-C strip=none -C link-arg=--import-undefined" cargo build --release --target wasm32-unknown-unknown`,
  produces exactly the right artifact shape: imports `env::{CVT_assert, CVT_assume,
  CVT_sanity, CVT_rule_location, CVT_nondet_u64/i64/usize, CVT_calltrace_*}`, exports
  `memory` + each `#[rule]` symbol by name.
- **`-C link-arg=--import-undefined` is required on a modern nightly.** CVLR's
  `link(wasm_import_module="env")` is gated on `target_os = "none"`, but
  `wasm32-unknown-unknown` is `target_os = "unknown"`, so the `CVT_*` symbols are plain
  undefined and `rust-lld` errors without the flag. stellar never hit this because it
  pins `nightly-2024-11-22`, which allowed undefined wasm symbols by default.
- **`#[rule]` requires a `std` crate.** `cvlr_rule_location!()` expands to `std::file!()`
  (`cvlr/cvlr-log/src/core.rs:243-248`). In `#![no_std]` it will not compile. This is why
  all of stellar-contracts uses `cvlr_soroban_derive::rule` instead of `cvlr::rule`.
- Unmodified Redis C **does** compile to wasm: `zig cc --target=wasm32-wasi -mcpu=mvp`
  builds 104 of 127 `redis/src/*.c` with zero source changes; the 23 failures are exactly
  the files calling fork/exec/signal/pthread/dlopen. A module linking real `sds.c` and
  exporting a rule over `env::CVT_*` exists and parses correctly — but it still imports
  29 `wasi_snapshot_preview1` functions, so it is **not** freestanding yet.
- `redis/src/redis-server` is built here (8.9.241, `malloc=libc`) and usable as an oracle.
- `nightly-2024-11-22` is installed and active.
- **No Certora tooling is installed**: no `certoraRun`, no `certora_cli`, no `~/.certora`,
  `CERTORAKEY` unset. Nothing can be run through the Prover from this machine as-is.

## CVLR: what to use, what to avoid

Use the plain vocabulary — it is what all 484 rules in the corpus use:
`cvlr_assert!`, `cvlr_assume!`, `cvlr_assert_eq/ne/le/lt/ge/gt!`, the `_if` guarded
variants, `clog!`, `nondet::<T>()`, `#[derive(Nondet, CvlrLog)]`, `#[rule]`.

**Avoid / know the traps:**

- `cvlr_rules!`, `cvlr_invariant_rules!`, `cvlr_rule_for_spec!` expand to a call to
  `cvlr_impl_rule!`, which **CVLR does not define** — the user must supply it. It is
  defined only inside cvlr's own tests (`cvlr-spec/tests/test_spec.rs:1138`). Zero uses
  across all five stellar-contracts branches. Usable if you write the ~12-line macro, but
  you would be the first.
- The whole `cvlr-spec` DSL (`CvlrFormula`/`CvlrSpec`/`CvlrLemma`/`cvlr_def_predicate!`)
  has **zero production usage**: stellar pins `cvlr = "0.4.0"`, and cvlr-spec first shipped
  in 0.6.0.
- `chandra/invariant-macros@origin` in stellar-contracts looks like a better parametric
  invariant idiom (`CvlrProp` + `base_*` + `impl_cvlr_rule_for_bases!`). **It is WIP and
  does not work**: its workspace lists a `certora/merkle_distributor` member that does not
  exist (so cargo fails at manifest resolution), no `.conf` references any generated rule
  name, and several properties are stubbed `cvlr_assert!(false)` with the real `cvlr_inv!`
  formulations commented out. Read it for the *idea*; do not treat it as a working pattern.
- The `macro_rules!` spec macros take the context type as `$ctx: ident`, not a type — so
  no generics, no paths, no references. Context structs must be bare, non-generic, in scope.
- A two-state predicate's single-state methods are poisoned with `cvlr_assert!(false);
  panic!()`. Handing one to `cvlr_invar_spec!` (whose `check_ensures` calls `.assert()`)
  fails unconditionally. **Invariants must be single-state.**
- `CvlrLemma::apply()` is the dual of `verify_with_context()` — it *asserts* requires and
  *assumes* ensures. Mixing them up turns an obligation into an unsound assumption.
- The optional description argument to `cvlr_assert!`/`cvlr_assume!` is parsed and
  **silently discarded**. Use `clog!` for anything you want in a counterexample.
- `cvlr_assume!` emits no location info; `cvlr_assert!` does.
- CVLR has **no f32/f64 at all** — no `Nondet`, no `CvlrLog`, no conversion. ZSET scores,
  `INCRBYFLOAT`, GEO, HLL cannot be modeled directly.
- CVLR has **no threads, locks, memory ordering, happens-before, or scheduler primitive**
  anywhere in its ~15k lines. Concurrency must be reified into an explicit abstract state.
- `nondet::<bool>()` costs a full symbolic u64; `#[derive(Nondet)]` on an enum draws a u64
  per value. Nondet-heavy contexts get expensive fast.
- **Draw bounded values, do not assume them.** `nondet::<u64>() % K` keeps concrete-mode
  fuzzing viable; `cvlr_assume!(k < K)` rejects essentially every concrete run.

## Spec idioms from the corpus (`stellar-contracts`, branch `port`)

Taxonomy — one file per category, `<subject>_<category>.rs` under `src/**/specs/`:

| category | shape |
|---|---|
| `_integrity` | relate post-state to pre-state for one function; no invariant assumed |
| `_invariants` | one rule per (invariant × mutating function), via `assume_pre_X` / `assert_post_X` helpers |
| `_panics` | end in `cvlr_assert!(false)`; passes iff the call always traps |
| `_non_panics` | end in `cvlr_assert!(true)`; need `"prover_args": ["-trapAsAssert", "true"]` |
| `_contract` | not rules — a `#[contract]` harness exposing entry points |

Header comment convention (keep it, it is how properties stay reviewable):

```rust
// property: P-XX. <Name>.
// description: <what it says>
// status: verified | vacuity issue! | ...
```

**Rule names are global across the compiled wasm.** The corpus avoids collisions with
manual prefixes (`fungible_`, `wt_`, `sl_`, `nft_`). Pick a prefix convention up front.

**Corpus antipatterns — do not copy:**
- `cvlr_satisfy` is imported in 51 of 55 spec files and invoked **zero** times.
- `fungible_invariants.rs:110` places an assumption *after* the mutating call, which can
  silently exclude the counterexample the rule exists to find. The file admits the
  invariant is unprovable as written.
- `vault_64_solvency.rs` declares the vault's headline safety property in a comment,
  defines `assume_pre_solvency`, and contains **zero** `#[rule]`s. It is assumed, never proved.
- `math_rounding` is verified for a *simplified* program (`virtual_offset` hardcoded to 1).
- `.conf` files are **not strict JSON** (trailing commas); `global_timeout` is sometimes a
  string, sometimes an int. Confs and source drift — one conf names a rule that does not exist.
- Bounded nondet containers (MAX=5) combined with `loop_iter: 1..4` and
  `optimistic_loop: true` mean coverage is **narrower than it looks**: `optimistic_loop`
  *assumes away* every execution exceeding the bound.

## Redis 8.9.241: facts a spec author must not get wrong

**This is not stock Redis 7/8 OSS. Do not write the spec from memory of upstream Redis.**
The value is a `kvobj` embedding the key string *and* the expire timestamp
(`object.h:31-66`); `db->keys` and `db->expires` are `kvstore`s holding the *same*
pointers. There are new types (`OBJ_ARRAY`), encodings (`LISTPACK_EX`, `TMPL_LP`),
policies (`volatile-lrm`), and commands (`DELEX`, `HSETEX`, `SET IFEQ`, `BLESS`, `AR*`).

- **"A key past its TTL is never observed" is FALSE as stated.** It holds only for the
  `lookupKey*` path. The correct statement is: *no command that reaches its key through
  `lookupKey*()` returns the value of a key whose expire < `commandTimeSnapshot()`.*
  **Measured on the built binary** (see the oracle recipe below), for one key with an
  elapsed `PX`:

  | observation | result | deletes? |
  |---|---|---|
  | `DBSIZE` | `1` | no — no expiry filter at all (`db.c:3148-3173`) |
  | `INFO keyspace` | `db0:keys=1,expires=1` | no (`server.c:7272-7283`) |
  | `KEYS *` | `[]` | **no** — filters with `keyIsExpired`, leaves the key (`db.c:1638-1645`) |
  | `EXISTS k` | `0` | **yes** — goes through `lookupKey*`, DBSIZE then drops to 0 |
  | `HLEN h` | `2` | no — counts expired fields, while `HGETALL` returns 1 (`t_hash.c:5596-5603`) |

- **Expiry is ~seven-way conditional, not two-way.** `expireIfNeeded` (`db.c:3004-3065`)
  can return *without deleting* via: `asmIsKeyInTrimJob` → KEY_VALID or KEY_TRIMMED
  (3011-3019); `flags & EXPIRE_ALLOW_ACCESS_EXPIRED` (3022); `server.loading ||
  server.allow_access_expired` inside `keyIsExpired` (2948); the replica / cluster branch
  — note the guard is `server.masterhost != NULL || server.cluster_enabled`, so **cluster
  mode alone triggers it** — with `CLIENT_MASTER` → KEY_VALID (3043-3045);
  `!confAllowsExpireDel()`, gated on the `lazyexpire-nested-arbitrary-keys` config
  (3050-3051); `flags & EXPIRE_AVOID_DELETE_EXPIRED` (3055); and
  `isPausedActionsWithUpdate(PAUSE_ACTION_EXPIRE)` (3062). Several fire on a plain
  standalone master. Any `exists` predicate must be parameterized by (role, client flags,
  loading, pause state, cluster_enabled, config).
- `RANDOMKEY`'s give-up-after-100-tries branch is **conditional**, not unconditional: it
  requires `allvolatile && (server.masterhost || isPausedActions(PAUSE_ACTION_EXPIRE))`
  (`db.c:855-865`). Measured on a standalone master with one expired volatile key:
  `RANDOMKEY` → nil, `DBSIZE` → 0. Do not cite it as a general counterexample.
- Lazy expiry uses `now > when`; active expiry uses `now >= when`. They disagree at
  exactly `t == expire`. Hash-field TTL uses a third comparison.
- **The atomic unit is the execution unit, not the command** (`server.execution_nesting`,
  `server.c:1420-1432`): one top-level `call()`, one whole `EXEC`, or one whole script.
  Propagation flushes only at nesting 0. Modeling atomicity per-command gives wrong
  replication predicates.
- **Redis 8.9 is not single-threaded.** Up to 128 IO threads, each with its own event
  loop, do full RESP parsing, command lookup, arity checks and socket writes. They never
  execute commands (`networking.c:3927-3934`). The correct invariant is narrower: *command
  execution and keyspace mutation* are single-threaded on the main thread. `io-threads` is
  an IMMUTABLE config and defaults to 1.
- **Script atomicity is conditionally broken**: past `busy-reply-threshold` ms a script
  re-enters the event loop and other clients' commands run between its `redis.call()`s.
- **There is no rollback anywhere.** `MULTI/EXEC` is isolation, not rollback — only
  queue-time errors abort. A partially-applied script cannot be killed.
- `EXEC` has three distinct outcomes that must not be conflated: `-EXECABORT` (queue-time
  error), RESP null array (WATCH invalidated), and a normal array whose elements may be errors.
- **WATCH is key-name-based, not value-based** — writing a key back to its original value
  still invalidates. One exemption: a key already logically expired at WATCH time.
- **`FLUSHALL SYNC` is silently upgraded to ASYNC** (`db.c:1370-1373`): the keyspace empties
  immediately but the caller's `+OK` waits for the bio lazy-free queue. Not inside MULTI/script.
- **Blocking commands have two contracts**: inside MULTI/script they do not block —
  `BLPOP key 0` returns a null array.
- `server.dirty` is a hand-maintained global counter across ~135 scattered increments, and
  it alone decides propagation. `(keyspace changed) <=> (dirty delta > 0)` is arguably the
  single highest-value invariant to formalize.
- **Durability is a property of the event loop, not the command.** `flushAppendOnlyFile`
  runs at `server.c:2070`, `handleClientsWithPendingWrites` at `server.c:2117`. That
  ordering *is* the contract, and it only means "fsynced before reply" under
  `appendfsync always`.
- `WAIT` counts replicas that *applied* to memory, not fsynced, and says nothing about
  which replicas are eligible for election.
- **Eviction is user-visible nondeterminism**: `performEvictions()` runs synchronously
  *before every command* when `maxmemory != 0`, can delete keys across all DBs, and is
  explicitly approximate/randomized. Either pin `maxmemory=0` or model arbitrary key loss.
- **A read command can be a write**: `SCAN` and `GET` lazily expire and propagate `DEL`s.
- `cluster_asm.c` (~3,900 lines, Atomic Slot Migration) is new in this version and is a
  much larger concurrency surface than classic cluster. Treat as a separate project.

### Redis's own specification artifacts (reuse these)

- `redis/src/commands/*.json` — 464 entries, 442 with `command_flags`, 433 with
  `reply_schema`, 240 with `key_specs`. **Disclaimed as non-authoritative** by
  `src/commands/README.md`; 7 have INCOMPLETE key specs and 3 (including `SET`) have
  VARIABLE_FLAGS. Good for a coverage matrix and type signatures; supplies no
  preconditions, error contracts, or cross-command invariants.
- `tests/` (tcl) — the executable oracle. Three directly reusable predicates:
  `DEBUG DIGEST` / `debug_digest_value` (state equality), `csvdump` (canonical
  serialization), and `attach_to_replication_stream` / `assert_replication_stream` (the
  exact emitted command sequence). The last is the mechanised form of every propagation
  property. Note the suite runs under `tests/assets/default.conf`, **not** production defaults.
- `MANIFESTO` states explicit non-guarantees; `redis.conf` comments document the
  durability/consistency tradeoffs.
- There is **no** in-tree linearizability checker, Jepsen-style harness, or any formal-methods
  artifact of any kind. Any real concurrency property needs new machinery.

## Scope discipline

The main failure mode for this project is unbounded scope. Any spec must write down and
defend a fence, e.g.: standalone (cluster off), one DB, `maxmemory=0`, a named fsync mode,
no modules/Lua/ACL, no floats, K ≤ 5 keys, small client and step counts. `src/config.c`
defines 222 configs of which roughly 40 change observable semantics rather than performance.

Bounded means bounded: with `optimistic_loop: true`, "for all keys" means "for all three
keys", and a passing rule means "no counterexample within the bound".

## Open gate

Neither candidate path can be validated from this machine — there is no Prover access here.
The first milestone of *any* approach should be getting one trivial rule to a verdict
(and confirming a deliberately false rule produces a real counterexample), before writing
significant model or harness code.

## Decisions (2026-09-19)

**Target: Rust model + property catalog.** `redis-cvlr/` becomes a Cargo workspace holding
(a) `PROPERTIES.md`, a numbered catalog in the corpus `P-XX` style, every entry anchored to
cited Redis C; and (b) an explicit abstract Redis state machine in Rust with CVLR `#[rule]`s
over it. The C tree is read-only reference material and an oracle process — never compiled,
never linked, never seen by the Prover. State this on page one of `PROPERTIES.md`: **what is
proven is proven about the model.**

**Prover access is obtainable but not on a known timeline.** Two consequences, both binding:
1. The catalog must stand alone as a reviewable artifact. Its value does not depend on a
   Prover run, and it is the deliverable that lands first.
2. **Differential fuzzing is primary evidence, not a secondary leg.** Build the dual-mode
   driver early: our own `#[no_mangle] extern "C-unwind" CVT_*` over a seeded byte stream
   lets the *same* `#[rule]` functions execute concretely, replaying each drawn step
   sequence against the already-built `redis/src/redis-server` over RESP and comparing
   replies plus a `csvdump`-shaped snapshot. This is the discipline that forces
   "draw bounded values, never `cvlr_assume!` them".

**Scope, first iteration: keyspace + expiry.** Strings, `DEL`/`EXISTS`/`TYPE`, the full
`EXPIRE` family, and the expiry-visibility split. Pinned config: standalone, cluster off,
one DB, `maxmemory=0`, `appendonly no`, `save ''`, no modules/Lua/ACL, no floats, K ≤ 5
opaque keys, 8-byte string values, active expiry modeled as an explicit adversarial step.

**Concurrency: all four dimensions are in the catalog; only some are encodable.** The user
selected all four knowing CVLR cannot discharge two of them. Do not silently drop those —
carry each as a catalog entry with an explicit status and the tool that *could* discharge it.

| dimension | catalog status |
|---|---|
| Client-visible contracts (atomicity boundary, WATCH/CAS, blocking FIFO, expiry visibility, MULTI framing) | **encodable** — reify the schedule into the abstract state, quantify over bounded interleavings |
| Replication / durability anomalies | **partly encodable** — safety over an effect log; all liveness (eventual convergence) is not expressible |
| Thread-safety / data races | **NOT expressible in CVLR** — record the requirement, cite the specific race (`multi.c` `REDIS_NO_SANITIZE`, IO threads dereferencing live keyspace `robj`s), name TSan / a memory-model tool as what would discharge it |
| Cluster / atomic slot migration | **deferred** — record the surface and the new `-TRYAGAIN Slot is being trimmed` contract; `cluster_asm.c` is its own project |

A catalog entry that names an unmet requirement and why it is unmet is more useful than an
omission. Every entry carries: ID, English statement, CVLR form (`integrity` | `invariant` |
`panic` | `non_panic` | `interleaving` | `satisfy` | `NOT-EXPRESSIBLE`), the Redis `file:line`
evidence, scope caveats (role, caller kind, maxmemory, cluster), and a status.

## Running the real server as an oracle

`redis/src/redis-server` is already built (8.9.241, 17 MB). **`redis-cli` is NOT built** —
speak RESP directly (a ~15-line python socket client is enough; there is one in the
scratchpad). Launch it so the keyspace is quiet and `DEBUG` is available:

```
redis/src/redis-server --port 7799 --save '' --appendonly no \
    --enable-debug-command yes --logfile /tmp/r.log
```

`--enable-debug-command yes` matters: without it `DEBUG SET-ACTIVE-EXPIRE 0` fails and the
active expire cycle reaps test keys within ~100 ms, so every expiry experiment silently
gives the wrong answer. The table above was measured under exactly this configuration.

## Risks that are not yet retired

- **The "known-good" Rust path is unexercised with this cvlr.** stellar-contracts pins
  `cvlr = "0.4.0"` from crates.io and its `Cargo.lock` contains **zero** cvlr entries; it
  imports `rule` from `cvlr_soroban_derive`, never `cvlr::rule`. So nothing in this
  workspace has ever been built against the local cvlr 0.6.1, and cvlr's own `#[rule]` has
  never reached the Prover from here. (It *does* compile and link correctly to wasm — that
  part is byte-verified.)
- **The Soroban fallback may not be reachable offline.** If the answer to the Prover
  question is "wrap it in a Soroban shell and use `cvlr_soroban_derive::rule`", that crate
  is a git dependency on branch `soroban-22.0.8` that is not vendored and has no
  `~/.cargo/git` checkout here. Nobody has checked whether it can be fetched.
- **Prover capacity for this shape of problem is completely unestimated.** Every
  bounded-interleaving plan is K keys × C clients × N steps, `nondet::<bool>()` costs a
  full symbolic u64, and stellar's heaviest confs already need `global_timeout 7200` /
  `smt_timeout 6000` / `-split false` for far simpler contracts. Expect to find the ceiling
  empirically.
- **The 8.9.241-only surface was excluded by default, and that may be wrong.** `DELEX`,
  `INCREX`, `HSETEX`, `HIMPORT`, `BLESS`, `GCRA`, `OBJ_ARRAY`, `SET IFEQ`, compact hashes —
  none have an upstream analogue. If this work is *for* the pre-release, that surface may be
  exactly what needs specifying. Confirm before excluding.

## Style models and starting points

- **Style model:** `stellar-contracts/packages/access/src/ownable/specs/` — the complete
  integrity/invariants/panics/non_panics quartet with real `P-NN` numbers.
- **Smallest complete example:** `packages/contract-utils/src/pausable/specs/` (10 rules).
- **Do NOT use as a reference:** `packages/tokens/src/rwa/specs/` — self-labelled
  `status: violated` and `// DUPLICATE CODE`.
- **Conf starting points:** `packages/contract-utils/confs/pausable_non_panics.conf`
  (minimal) or `packages/tokens/confs/vault_64_invariants.conf` (richest).
- **Rule-name prefixes for `redis-cvlr`** (names are global symbols): `expire_`, `string_`,
  `keyspace_`, `multi_`, `blocked_`, `propagate_`.
- `-trapAsAssert true` is **required** for any non-panic rule. Without it a Rust trap is
  treated as an assumed-away path and `cvlr_assert!(true)` holds vacuously.

## Leads worth chasing

- **`touchWatchedKey` list abandonment at `src/multi.c:415` — see `redis-cvlr/FINDINGS.md`
  F-01. Status: NOT REPRODUCED.** The anomaly is real (its sibling
  `touchAllWatchedKeysInDb` uses `continue` in the identical branch, multi.c:467/472) but a
  client-visible repro was attempted and **failed**: any write path that lazy-expires the
  key first calls `touchWatchedKey` while the key is absent, which clears `wk->expired`
  (multi.c:413) before the key becomes present, so the `break` is never reached. Do not
  report it as a bug. The one open route is `SWAPDB`, which sets `wk->expired = 1` with the
  key *present* (multi.c:472-474). Full analysis and the untried candidates are in FINDINGS.md.
- `(keyspace changed) <=> (server.dirty delta > 0)` — ~135 hand-maintained increments decide
  all propagation. Highest-value single invariant to formalize.

## Deferred (revisit later)

**The 8.9.241-only command surface is out of scope for now, by decision (2026-09-19).**
Not excluded because it is unimportant — excluded to keep iteration 1 finishable. Revisit:
`DELEX`, `INCREX`, `MSETEX`, `HSETEX`/`HGETEX`/`HGETDEL`, `HIMPORT` + compact hashes,
`SET IFEQ`/`IFDEQ`, `BLESS`, `GCRA`, `HOTKEYS`, the `AR*` array family and `OBJ_ARRAY`,
`LMOVEM`/`BLMOVEM`, `SUNIONCARD`/`SDIFFCARD`, `BACKUP`, `XREAD MAXCOUNT/MAXSIZE`, and the
new encodings (`LISTPACK_EX`, `TMPL_LP`, `TMPL_ARRAY`, `SLICED_ARRAY`) and policies
(`volatile-lrm`, `allkeys-lrm`). Several have no public documentation, so the C and the
tcl suite are the only sources. If this work is ultimately *for* the pre-release, this
surface is likely the highest-value part and should be promoted, not dropped.

## Differential testing (`redis-cvlr/tests/difftest.rs`)

The only evidence in this project that touches the shipped C. It spawns `redis-server`,
replays drawn schedules against both the model and the server, and compares every reply
plus the end-state snapshot. Two disciplines make it non-flaky:

- **TTLs are drawn far from boundaries** — `PAST` (-10 s), `SOON` (+30 ms), `FAR` (+10 min)
  — so execution drift never changes the sign of "is it expired". `ClockTick` is a real
  60 ms sleep, which decisively crosses `SOON` and nothing else.
- **Active expiry is disabled** (`DEBUG SET-ACTIVE-EXPIRE 0`) so the only deletions are the
  lazy ones the model also performs. This requires `--enable-debug-command yes`; without it
  the DEBUG call fails and every expiry comparison silently gets the wrong answer.

The snapshot is read in a non-mutating order: `DBSIZE` and `KEYS` first (pure
observations), then `GET` only for keys `KEYS` already reported live — so the measurement
cannot trigger the lazy delete it is trying to observe. `PTTL` is compared by **bucket**
(-2 / -1 / ≥0), never by value: the two clocks differ by however long the schedule ran.

Always keep an injected-bug test alongside it. A harness that has never caught anything is
not known to work.

### What it has caught

**`SET k v PXAT <elapsed>` writes nothing and deletes** (t_string.c:161-175). An
already-elapsed absolute expire takes an early-return branch — ordered *after* the NX/XX
check — that deletes any existing key, propagates `DEL` rather than `SET`, replies `+OK`,
and never writes the value. 102 of 400 schedules diverged on this.

The important part: property **P-06 had been stated wrongly** ("`SET ... PXAT t` installs
exactly `t`"). The error was in the *specification*, so proving the model would never have
surfaced it. This is the argument for the differential leg in one example.

## Iteration 2: transactions and the replication-stream oracle

**Propagation is a two-stage buffer, and it has to be.** Effects go to `World::pending` via
`also_propagate` during a unit; `exit_execution_unit` flushes them to `World::repl` at
nesting 0, adding `MULTI`/`EXEC` framing iff more than one op was emitted. Pushing straight
to `repl` would make the framing rule (P-13) unstateable. A consequence: **rules must call
`step::dispatch`, never `cmd::exec_cmd`** — the latter is an internal that skips the unit
boundary, so nothing is ever framed or flushed and propagation assertions silently see an
empty log.

**The replication-stream oracle** (`resp.rs::sync_start` / `drain_propagated`) is Redis's
own `attach_to_replication_stream`. `SYNC` returns `$<len>\r\n<rdb bytes>` with **no**
trailing CRLF, so the payload must be skipped by length rather than parsed as a bulk
string; after that the socket carries propagated commands as ordinary RESP arrays. Filter
`SELECT`, `PING` and `REPLCONF` — the master emits those on its own schedule. Compare
command *names and framing*, not full arguments: `DEL` vs `UNLINK` depends on the lazyfree
config and Redis rewrites relative TTLs to absolute `PXAT`.

**Coverage counters are not optional.** The first transaction-aware difftest run reported
400/400 agreeing — while `exec_nil` was 0 and `framed_units` was 2. The two properties that
mattered most were essentially never exercised. A uniform random generator reaches neither:
a transaction only gets framed if it emits ≥2 *writes*, and a WATCH is only invalidated if
another client writes *that* key. Both need deliberate bias, and the assertions in
`tests/difftest.rs` now fail the run if any path count is zero. Treat "N/N agreed" without
coverage numbers as unverified.

**`UNWATCH` is not `unwatchAllKeys`.** The `UNWATCH` *command* clears `CLIENT_DIRTY_CAS`;
the `unwatchAllKeys` *helper* does not, because `touchWatchedKey` calls it immediately
after setting that flag. Model them as two functions or WATCH silently stops invalidating.

**Scale the difftest sample before believing it.** D-02 was invisible at 400 schedules ×
12 steps and appeared 29 times at 2500 × 28 — it needs a dirtying write, then `UNWATCH`,
then a whole second transaction. `DIFFTEST_SCHEDULES` / `DIFFTEST_LEN` exist for this.

**A single command can propagate as a framed transaction.** `SET` onto a logically-expired
key emits the lazy-expire `DEL` plus the `SET`, so the wire shows `MULTI DEL SET EXEC`.
Confirmed against the real server. Anyone modelling "one command = one propagated command"
will be wrong.

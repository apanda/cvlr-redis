# redis-verus — the Verus leg

A second verification leg alongside `../redis-cvlr`. **Not a port and not a replacement.**
It exists to state properties CVLR/Sunbeam structurally cannot: unbounded quantification,
induction over arbitrary-length command sequences, and loops discharged by invariant rather
than by bounded unrolling.

## What is proved today

Run `just verify` (24 proofs, ~1s incremental).

- **Well-formedness over all reachable states.** No absent key carries a TTL, in every
  state reachable by *any* sequence of commands, on a keyspace of *any* size. The CVLR
  analogue (P-05) assumes the invariant, runs one drawn command, and re-asserts it, at
  K = 3, with `optimistic_loop` assuming away anything longer.
- **No resurrection.** Over any sequence of reads, clock ticks and expire cycles, the set
  of physically present keys is monotonically non-increasing. Not in the catalog at all —
  it cannot be phrased in a bounded single-execution prover.
- **Active expiry (P-11) as a loop invariant**, for any number of keys: never creates a
  key, never alters a survivor, deletes only keys whose TTL has elapsed. CVLR approximates
  this with 15 hand-written assertions at K = 3.
- **P-10 / P-10b** as executable post-state properties, with non-vacuity witnesses.

## What is NOT here, and why it matters

- **No values.** `Slot` is `{ present, expire_at }`; the CVLR model has a `value` field and
  this one dropped it. Blocks `SET`, and weakens anything about what a read returns.
- **No `SET`, no watch table.** So replication refinement, TTL rewriting, read-after-write,
  idempotence and the WATCH properties are all out of reach until those land.
- **No differential wiring.** This is the important one. Two transcriptions of the same C
  can drift, and nothing currently detects it. The model is deliberately executable
  (`Vec`/`i64`, `exec fn`s, compilable by `verus --compile`) so it can be driven by
  `../redis-cvlr/tests/difftest.rs` against a real redis-server. Until that exists, treat
  agreement between the two legs as unverified.

## Verus does not produce counterexamples

It reports "postcondition not satisfied at line N". `--expand-errors` decomposes the
failing predicate and marks which conjunct broke, which is usually enough to localise. But
a failed proof means *could not prove*, not *is false* — the property may be true and the
invariant too weak. That is why `tools/mutate.sh` and the executable witnesses matter more
here than they would with a counterexample-producing prover.

## The mutation check

`just mutate` injects known bugs one at a time and confirms each breaks verification.
A verification suite that has never rejected anything is not known to work. Current
battery includes the aliasing bug (two keys resolving to one slot) and the `when < 0`
faithfulness regression that shipped in the CVLR model and survived 20 000 concrete
executions plus 3000 differential schedules.

Note the battery is hand-chosen with knowledge of what the proofs assert, which biases
toward catchable bugs. A mechanical mutation pass measuring kill rate would be stronger.

## Layout assumption

`Cargo.toml` has path dependencies on `../../verus/source/{vstd,builtin,builtin_macros}`,
so a Verus checkout must sit beside `cvlr-redis`. That is a HARD build dependency: without
it `cargo` cannot resolve and nothing verifies.

This crate does **not** need a Redis checkout. Every `redis/src/...` reference in the
sources is a citation in a doc comment; nothing opens a file.

The Verus binary path is configurable: `justfile` and `tools/mutate.sh` both read
`VERUS_BIN`, defaulting to `../../verus/source/target-verus/release`.

Note this Verus renamed `builtin` to `verus_builtin`, so the layout in
`verus/examples/cargo-verus/` is out of date and will not resolve.

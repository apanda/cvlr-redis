# redis-cvlr

A CVLR specification of Redis's user-visible behavior. Iteration 1: **keyspace + expiry**,
standalone.

| file | what |
|---|---|
| `PROPERTIES.md` | the numbered property catalog — **start here** |
| `FINDINGS.md` | open leads and their evidence status |
| `src/model/` | abstract state machine, every fn citing the `redis/src` range it transcribes |
| `src/specs/` | the `#[rule]` functions |
| `src/driver/concrete.rs` | `CVT_*` implementations so the same rules run without a Prover |
| `confs/` | Certora Prover configurations |
| `../CLAUDE.md` | toolchain, idioms, and the traps |

## What this does and does not establish

**It proves things about a model, not about Redis.** CVLR is Rust-only; the Prover ingests
a wasm32 module built by cargo; Redis is C. `src/model/` is a hand transcription of cited C.
The correspondence argument lives in the differential driver and in line-level
traceability, not in the Prover. See PROPERTIES.md § "Read this first".

Nothing has been through the Prover — there is no Certora tooling on this machine. Every
property is `unproven`. **A rule that compiles is not a verified rule.**

## Running it

Without a Prover — executes every rule body over 20 000 seeded streams:

```
cargo test --features "certora,rt" --release --test concrete -- --nocapture
```

`multi_touch_dirties_every_watcher` is **expected to fail**; it targets FINDINGS.md F-01.
The test asserts that it fails, so if it starts passing the test breaks on purpose.

Differential testing against a real `redis-server` — the only evidence here that touches
the shipped C. It spawns the server itself and skips (does not fail) if it is not built:

```
cargo test --features "certora,rt" --release --test difftest -- --nocapture --test-threads=1
DIFFTEST_SCHEDULES=5000 DIFFTEST_LEN=24 cargo test ...   # scale the sample up
```

Three tests run: the schedule comparison, an injected-bug check (a harness that cannot
fail proves nothing), and the FINDINGS.md F-01 reachability probe.

Building the Prover artifact, and checking its shape without a Prover:

```
just build
just check-wasm
```

`check-wasm` confirms the module imports only `env::CVT_*` and exports every rule symbol by
name. A rule missing from the export list will never be found by the Prover.

With a Prover (untested — no access here):

```
certoraRun confs/keyspace_expiry.conf
```

## Two rules that are not style preferences

**Draw, do not filter.** `nondet::<u64>() % K`, never `cvlr_assume!(k < K)`. The assume form
makes the same rule bodies useless in concrete mode — it rejects essentially every run. The
concrete pass currently executes 20 000/20 000 with 0 rejected; that number is the health
check.

**Watch the loop bounds.** `optimistic_loop: true` *assumes away* every execution that
iterates past `loop_iter`, so an over-long loop makes a rule silently vacuous rather than
failing. Any loop in the model must stay under the conf's `loop_iter`, or the conf must be
raised and `optimistic_loop` turned off. This is why `draw_str` has no byte loop.

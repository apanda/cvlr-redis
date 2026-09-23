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

With a Prover — **run from inside `confs/`**, because `build_script` resolves relative to
the working directory, not to the conf file:

```
cd confs
certoraSorobanProver keyspace_expiry.conf --wait_for_results all
```

`certoraSorobanProver`, not `certoraRun` (that one is EVM-only). Without
`--wait_for_results all` you get a job link and no verdicts in the terminal.

**Set `opt-level` in `Cargo.toml` to match the conf before running.** There is one profile
and `certora_build.py` always runs `just build`, so the value decides which conf can
reproduce its result:

| conf | needs |
|---|---|
| `keyspace_expiry.conf` | `opt-level = 3` — at `"z"` all 12 rules exceed `MaxBlockCount` |
| `findings.conf` (F-01) | `opt-level = "z"` — at `3` the rule's `ProverInternalChecks` fails |

#### Why the optimization level decides this

`loop_iter` bounds how many times the Prover unrolls each loop, so what matters is the
number of loops that survive codegen, **not** how big the module is.

`-O3` fully unrolls five small constant-trip loops away — 16 loops become 11. That is what
keeps the catalog under `MaxBlockCount`. At `"z"` all 16 survive, every one of them gets
unrolled `loop_iter` times, and the twelve catalog rules blow the ceiling (~112k > 100k)
and report `UNKNOWN` before the solver ever runs. Byte size points the other way and is
misleading: the `"z"` module is 12 KB against 25 KB for `-O3`.

The same unrolling breaks F-01. `touch_watched_key` takes its loop bound from a memory load
(`w.watched.len[k]`) and exits via `return` from inside the body — the `multi.c:415`
`break`, transcribed as written. `-O3` rearranges the loop head enough that the Prover's
BMC can no longer identify an exit condition, and when it cannot it emits `assert false`
(EVMVerifier `decompiler/BMC.kt:263`). That fails the rule's `ProverInternalChecks` and
makes the verdict meaningless. At `"z"` the loop head survives intact and the unwinding
condition is discharged.

Also: do **not** add `#[inline(always)]` to the model on top of either setting. Measured
4.1x larger code (104,623 vs 25,498 bytes), and it fails under both.

#### This is a workaround, not the intended setup

Everything above is what makes the two confs run **today**, found empirically while getting
the first results out. None of it is a design decision and none of it should survive
unexamined. Having one `Cargo.toml` profile that has to be flipped by hand between confs is
a footgun, and it means neither result is reproducible from a clean checkout without
reading this section first.

Things worth trying when someone picks this up:

- a per-conf profile, so the build script selects one instead of the repo carrying a
  mutable global (a cargo feature, a second `[profile.release-*]`, or `certora_build.py`
  keying off the conf name);
- making `touch_watched_key`'s loop bound a compile-time constant (`while i < C` with the
  `break` as a flag), which should make it analyzable under both profiles and remove the
  reason the two confs disagree at all;
- raising `loop_iter` far enough that no unwinding assert is ever emitted, which would also
  make `multi_assert_check` safe to turn back on.

Until then, follow the table.

See `FINDINGS.md` for the F-01 result this reproduces.

Two more things the conf validator will reject without explaining: `msg` may not contain
`+`, `<` or `;`, and `multi_assert_check` should stay **off** for `keyspace_expiry.conf` —
with it on, Prover-injected loop-unwinding asserts get counted as user asserts and the run
dies with `IllegalStateException` (6 of them in one 13-rule job).

## Two rules that are not style preferences

**Draw, do not filter.** `nondet::<u64>() % K`, never `cvlr_assume!(k < K)`. The assume form
makes the same rule bodies useless in concrete mode — it rejects essentially every run. The
concrete pass currently executes 20 000/20 000 with 0 rejected; that number is the health
check.

**Watch the loop bounds.** `optimistic_loop: true` *assumes away* every execution that
iterates past `loop_iter`, so an over-long loop makes a rule silently vacuous rather than
failing. Any loop in the model must stay under the conf's `loop_iter`, or the conf must be
raised and `optimistic_loop` turned off. This is why `draw_str` has no byte loop.

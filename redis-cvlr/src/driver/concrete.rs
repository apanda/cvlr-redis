//! `#[no_mangle]` implementations of the CVT_* ABI, over a seeded pseudo-random stream.
//!
//! NOTE ON UNWINDING. cvlr declares these as `extern "C"`, and since Rust 1.81 a panic
//! escaping an `extern "C"` boundary aborts the process. So none of these may panic:
//! a failed assume sets REJECTED and a failed assert records a failure, rather than
//! unwinding. Rules keep running after a rejected assume, which is harmless because every
//! value is DRAWN in range rather than assumed into range.

#![allow(improper_ctypes_definitions)]

// cvlr declares the calltrace hooks with Rust `&str` parameters
// (cvlr-log/src/core.rs:10-28). That is not FFI-safe, but our definitions MUST match those
// declarations exactly or they will not link. The Prover never actually calls these with a
// foreign ABI, and in concrete mode both sides are this same Rust crate.

use core::cell::Cell;

thread_local! {
    static STREAM: Cell<u64> = const { Cell::new(0) };
    /// A `cvlr_assume!` failed: this execution is not a counterexample, discard it.
    static REJECTED: Cell<bool> = const { Cell::new(false) };
    /// A `cvlr_assert!` failed while not rejected: this IS a counterexample.
    static FAILED: Cell<bool> = const { Cell::new(false) };
    static DRAWS: Cell<u64> = const { Cell::new(0) };
}

/// xorshift64*. Deterministic, so a failing seed is a reproducible test case.
fn next_u64() -> u64 {
    STREAM.with(|s| {
        let mut x = s.get();
        if x == 0 {
            x = 0x9E3779B97F4A7C15;
        }
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        s.set(x);
        x.wrapping_mul(0x2545F4914F6CDD1D)
    })
}

/// Start a fresh execution with the given seed.
pub fn begin(seed: u64) {
    STREAM.with(|s| s.set(seed | 1));
    REJECTED.with(|r| r.set(false));
    FAILED.with(|f| f.set(false));
    DRAWS.with(|d| d.set(0));
}

pub struct Outcome {
    pub rejected: bool,
    pub failed: bool,
    pub draws: u64,
}

pub fn end() -> Outcome {
    Outcome {
        rejected: REJECTED.with(|r| r.get()),
        failed: FAILED.with(|f| f.get()),
        draws: DRAWS.with(|d| d.get()),
    }
}

#[no_mangle]
pub extern "C" fn CVT_nondet_u64() -> u64 {
    DRAWS.with(|d| d.set(d.get() + 1));
    next_u64()
}

#[no_mangle]
pub extern "C" fn CVT_nondet_i64() -> i64 {
    CVT_nondet_u64() as i64
}

#[no_mangle]
pub extern "C" fn CVT_nondet_usize() -> usize {
    CVT_nondet_u64() as usize
}

#[no_mangle]
pub extern "C" fn CVT_assume(c: bool) {
    if !c {
        REJECTED.with(|r| r.set(true));
    }
}

#[no_mangle]
pub extern "C" fn CVT_assert(c: bool) {
    // An assert reached on a rejected path proves nothing -- the Prover would never have
    // explored it.
    if !c && !REJECTED.with(|r| r.get()) {
        FAILED.with(|f| f.set(true));
    }
}

#[no_mangle]
pub extern "C" fn CVT_satisfy(_c: bool) {}

#[no_mangle]
pub extern "C" fn CVT_sanity(_c: bool) {}

#[no_mangle]
pub extern "C" fn CVT_calltrace_print_u64_1(_tag: &str, _x: u64) {}

#[no_mangle]
pub extern "C" fn CVT_calltrace_print_i64_1(_tag: &str, _x: i64) {}

#[no_mangle]
pub extern "C" fn CVT_calltrace_attach_location(_file: &str, _line: u64) {}

#[no_mangle]
pub extern "C" fn CVT_rule_location(_file: &str, _line: u64) {}

/// Run one rule over `n` seeds. Returns (executions, rejected, failed-seeds).
pub fn exercise(name: &str, f: fn(), n: u64) -> (u64, u64, Vec<u64>) {
    let mut rejected = 0u64;
    let mut failures = Vec::new();
    for seed in 1..=n {
        begin(seed);
        f();
        let o = end();
        if o.rejected {
            rejected += 1;
        } else if o.failed {
            if failures.len() < 8 {
                failures.push(seed);
            }
        }
    }
    let _ = name;
    (n, rejected, failures)
}

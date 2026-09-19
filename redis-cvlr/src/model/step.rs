//! Steps and schedules -- how concurrency is expressed at all.
//!
//! CVLR has no threads, locks, memory ordering, happens-before or scheduler primitive
//! anywhere in its ~15k lines. So the schedule is REIFIED into the state and quantified
//! over with nondet.
//!
//! AXIOM (not a theorem -- say so in any writeup): one `Step` is atomic. Justified by
//! citation, not proof: IO threads parse but refuse to execute and hand the client back to
//! the main thread (networking.c:3927-3934); clients that cannot be handled off-main are
//! pinned (iothread.c:286-292). The real atomic unit is the EXECUTION UNIT
//! (server.c:1420-1432), which also freezes the clock, and propagation flushes only at
//! nesting 0 (server.c:4060-4075). Because this is an axiom, no bug that violates it can
//! be found here.

use super::cmd::*;
use super::expire;
use super::state::*;

#[derive(Clone, Copy)]
pub enum Step {
    /// One command by one client = one execution unit.
    Cmd(ClientId, Cmd),
    /// The active expire cycle. NOT a background detail to be assumed away: an adversarial
    /// step the prover may insert anywhere, which turns "a key may vanish at any moment"
    /// into a hypothesis every other rule must survive.
    ActiveExpire,
    /// Time advances between units. Never *within* one.
    ClockTick(Ms),
}

/// Run one step. Opens and closes an execution unit around it.
pub fn step(w: &mut World, s: Step) -> Option<Reply> {
    match s {
        Step::Cmd(c, cmd) => {
            enter_execution_unit(w);
            let r = exec_cmd(w, c, cmd);
            exit_execution_unit(w);
            Some(r)
        }
        Step::ActiveExpire => {
            enter_execution_unit(w);
            expire::active_expire_cycle(w);
            exit_execution_unit(w);
            None
        }
        Step::ClockTick(d) => {
            w.clock += d;
            None
        }
    }
}

/// `enterExecutionUnit` (server.c:1420-1432). At nesting 0 the clock snapshot freezes, so
/// a whole EXEC or script sees one consistent time and no TTL can expire mid-transaction.
fn enter_execution_unit(w: &mut World) {
    w.nesting += 1;
    if w.nesting == 1 {
        w.dirty = 0;
    }
}

/// `exitExecutionUnit` + `postExecutionUnitOperations` (server.c:4060-4075).
fn exit_execution_unit(w: &mut World) {
    if w.nesting > 0 {
        w.nesting -= 1;
    }
}

// --------------------------------------------------------------- drawing schedules

pub fn draw_ttl_arg() -> TtlArg {
    match nondet_range(3) {
        0 => TtlArg::None,
        1 => TtlArg::Keep,
        _ => TtlArg::PxAt((cvlr::nondet::nondet::<u64>() % 2_000_000) as Ms),
    }
}

pub fn draw_set_cond() -> SetCond {
    match nondet_range(3) {
        0 => SetCond::Always,
        1 => SetCond::Nx,
        _ => SetCond::Xx,
    }
}

pub fn draw_expire_cond() -> ExpireCond {
    match nondet_range(5) {
        0 => ExpireCond::None,
        1 => ExpireCond::Nx,
        2 => ExpireCond::Xx,
        3 => ExpireCond::Gt,
        _ => ExpireCond::Lt,
    }
}

#[inline(always)]
pub fn nondet_range(n: u64) -> u64 {
    cvlr::nondet::nondet::<u64>() % n
}

/// Draw an arbitrary command. DRAWN, never filtered -- see CLAUDE.md.
pub fn draw_cmd(c: ClientId) -> Cmd {
    let k = draw_key();
    match nondet_range(10) {
        0 => Cmd::Set { key: k, val: draw_str(), cond: draw_set_cond(), ttl: draw_ttl_arg(), get: draw_bool() },
        1 => Cmd::Get { key: k },
        2 => Cmd::Del { key: k },
        3 => Cmd::Exists { key: k },
        4 => Cmd::Expire { key: k, at: (nondet_range(2_000_000)) as Ms, cond: draw_expire_cond() },
        5 => Cmd::Persist { key: k },
        6 => Cmd::Pttl { key: k },
        7 => Cmd::Keys,
        8 => Cmd::DbSize,
        _ => Cmd::Watch { client: c, key: k },
    }
}

pub fn draw_step() -> Step {
    let c = draw_client();
    match nondet_range(6) {
        0 => Step::ActiveExpire,
        1 => Step::ClockTick(nondet_range(1000) as Ms),
        _ => Step::Cmd(c, draw_cmd(c)),
    }
}

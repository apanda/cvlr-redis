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
use super::watch;
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
///
/// The queueing decision lives here, not in `exec_cmd`: in Redis it is made in
/// `processCommand` BEFORE `call()` ever runs (multi.c `queueMultiCommand`).
pub fn step(w: &mut World, s: Step) -> Option<Reply> {
    match s {
        Step::Cmd(c, cmd) => Some(dispatch(w, c, cmd)),
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

/// The command-dispatch path: queue it, or run it as its own execution unit.
pub fn dispatch(w: &mut World, c: ClientId, cmd: Cmd) -> Reply {
    if w.clients[c].in_multi && !cmd.is_txn_control() {
        // A command that fails at QUEUE time is the ONLY thing that aborts a transaction
        // (multi.c:110-125). Runtime errors do not.
        if cmd.is_bad() {
            w.clients[c].dirty_exec = true;
            return Reply::Error;
        }
        if w.clients[c].qlen < QCAP {
            let n = w.clients[c].qlen;
            w.clients[c].queue[n] = Some(cmd);
            w.clients[c].qlen += 1;
        }
        return Reply::Queued;
    }

    match cmd {
        Cmd::Multi => {
            // "ERR MULTI calls can not be nested".
            if w.clients[c].in_multi {
                return Reply::Error;
            }
            w.clients[c].in_multi = true;
            Reply::Ok
        }
        Cmd::Discard => {
            // "ERR DISCARD without MULTI".
            if !w.clients[c].in_multi {
                return Reply::Error;
            }
            w.clients[c].reset_txn();
            watch::unwatch_all(w, c);
            Reply::Ok
        }
        // "ERR WATCH inside MULTI is not allowed". WATCH is not queued, but it is also not
        // permitted once a block is open.
        Cmd::Watch { .. } if w.clients[c].in_multi => Reply::Error,
        Cmd::Exec => exec_transaction(w, c),
        Cmd::BadCommand => Reply::Error,
        _ => {
            run_unit(w, c, cmd)
        }
    }
}

/// Run one command as a complete execution unit.
fn run_unit(w: &mut World, c: ClientId, cmd: Cmd) -> Reply {
    enter_execution_unit(w);
    let r = exec_cmd(w, c, cmd);
    exit_execution_unit(w);
    r
}

/// `execCommand` (multi.c:184-238).
///
/// THE WHOLE TRANSACTION IS ONE EXECUTION UNIT. That is what makes it atomic to a replica
/// and what freezes the clock across sub-commands. Three distinct outcomes, which must not
/// be conflated:
///   -EXECABORT   a queue-time error occurred        (CLIENT_DIRTY_EXEC)
///   nil array    a WATCHed key was touched          (CLIENT_DIRTY_CAS)
///   array        normal -- individual ELEMENTS may be errors; runtime errors do NOT abort
fn exec_transaction(w: &mut World, c: ClientId) -> Reply {
    if !w.clients[c].in_multi {
        return Reply::Error; // EXEC without MULTI
    }
    if w.clients[c].dirty_exec {
        w.clients[c].reset_txn();
        watch::unwatch_all(w, c);
        return Reply::ExecAborted;
    }
    if w.clients[c].dirty_cas {
        w.clients[c].reset_txn();
        watch::unwatch_all(w, c);
        return Reply::ExecNil;
    }

    let qlen = w.clients[c].qlen;
    let queue = w.clients[c].queue;

    // ONE unit for the whole block.
    enter_execution_unit(w);
    let mut out = ExecResults::new();
    let mut i = 0;
    while i < qlen {
        if let Some(cmd) = queue[i] {
            let r = exec_cmd(w, c, cmd);
            out.push(r.as_sub());
        }
        i += 1;
    }
    exit_execution_unit(w);

    w.clients[c].reset_txn();
    watch::unwatch_all(w, c);
    Reply::ExecArray(out)
}

/// `enterExecutionUnit` (server.c:1420-1432). At nesting 0 the clock snapshot freezes, so
/// a whole EXEC or script sees one consistent time and no TTL can expire mid-transaction.
fn enter_execution_unit(w: &mut World) {
    w.nesting += 1;
    if w.nesting == 1 {
        w.dirty = 0;
        w.pending = ReplLog::new();
    }
}

/// `exitExecutionUnit` + `postExecutionUnitOperations` (server.c:4060-4075).
/// Propagation is flushed ONLY at nesting 0.
fn exit_execution_unit(w: &mut World) {
    if w.nesting > 0 {
        w.nesting -= 1;
    }
    if w.nesting == 0 {
        propagate_pending_commands(w);
    }
}

/// `propagatePendingCommands` (server.c:3993-4045).
///
/// The framing rule, and the headline atomicity property: a unit is wrapped in MULTI/EXEC
/// **iff it emitted more than one op**. One op goes bare; zero ops propagate nothing.
///
/// (The real rule has one more clause -- no framing if the direct command carries
/// `CMD_TOUCHES_ARBITRARY_KEYS`, which only `SCAN` and `RANDOMKEY` do, server.c:4014-4019.
/// Neither is modeled yet, so it is omitted rather than faked.)
fn propagate_pending_commands(w: &mut World) {
    let n = w.pending.len;
    if n == 0 {
        return;
    }
    if n > 1 {
        w.repl.push(Effect::Multi);
    }
    let mut i = 0;
    while i < n {
        if let Some(e) = w.pending.get(i) {
            w.repl.push(e);
        }
        i += 1;
    }
    if n > 1 {
        w.repl.push(Effect::Exec);
    }
    w.pending = ReplLog::new();
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

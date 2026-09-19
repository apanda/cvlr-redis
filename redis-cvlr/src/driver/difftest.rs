//! Differential testing: run a drawn schedule against both the model and a real
//! redis-server, and compare every reply plus the end-state snapshot.
//!
//! TIME IS THE HARD PART. The model has an explicit `clock`; the server has a wall clock
//! that advances while commands execute. Making them comparable without a flaky test needs
//! two disciplines:
//!
//! 1. TTLs are drawn at offsets FAR from any boundary the schedule can drift across --
//!    `PAST` (-10s), `SOON` (+30ms) and `FAR` (+10min). Execution jitter is well under 5ms,
//!    so the sign of "is it expired" is never in doubt.
//! 2. `ClockTick` is a real sleep of `TICK_MS` (60ms), which decisively crosses `SOON` and
//!    nothing else. The server's clock ends up marginally ahead of the model's, which is
//!    harmless given the margins.
//!
//! Active expiry is disabled on the server (`DEBUG SET-ACTIVE-EXPIRE 0`) so that the only
//! deletions are the lazy ones the model also performs. That is why `Step::ActiveExpire` is
//! excluded from difftest schedules -- there is no way to ask the server to run exactly one
//! cycle.

use crate::driver::concrete::begin;
use crate::driver::resp::{Client, Resp};
use crate::model::{cmd::*, state::*, step::*};

pub const TICK_MS: i64 = 60;
const PAST: i64 = -10_000;
const SOON: i64 = 30;
const FAR: i64 = 600_000;

fn key_name(k: KeyId) -> String {
    format!("k{k}")
}

/// The model draws values as one of a few distinct ids; render that as bytes for the wire.
fn val_bytes(s: &Str) -> Vec<u8> {
    s.as_slice().to_vec()
}

// ------------------------------------------------------------------ schedules

/// A schedule step for difftest. Deliberately narrower than `model::step::Step`:
/// `ActiveExpire` has no single-shot server counterpart, and WATCH/EXEC needs MULTI support
/// the model does not have yet (iteration 2).
#[derive(Clone, Copy)]
pub enum DiffStep {
    Cmd(ClientId, Cmd),
    Tick,
}

/// TTL offsets chosen to be unambiguous under drift. See module docs.
fn draw_ttl_offset() -> Option<i64> {
    match nondet_range(4) {
        0 => None,
        1 => Some(PAST),
        2 => Some(SOON),
        _ => Some(FAR),
    }
}

/// A command that definitely WRITES the given key. Needed for two reasons the coverage
/// counters made obvious: a transaction only gets MULTI/EXEC framing if it emits >1 op, and
/// a WATCH is only invalidated if somebody actually writes the watched key. Left to a
/// uniform draw, both paths are reached a couple of times in 400 schedules.
fn draw_write_on(k: KeyId, base: Ms) -> Cmd {
    match nondet_range(4) {
        0 => Cmd::Del { key: k },
        1 => Cmd::Expire { key: k, at: base + FAR, cond: ExpireCond::None },
        2 => Cmd::Persist { key: k },
        _ => Cmd::Set {
            key: k,
            val: draw_str(),
            cond: SetCond::Always,
            ttl: if nondet_range(2) == 0 { TtlArg::None } else { TtlArg::PxAt(base + FAR) },
            get: false,
        },
    }
}

/// Transaction bodies are write-biased, so units routinely emit more than one op and the
/// framing rule is actually exercised.
fn draw_txn_body(base: Ms) -> Cmd {
    if nondet_range(4) == 0 {
        draw_diff_cmd(base)
    } else {
        draw_write_on(draw_key(), base)
    }
}

fn draw_diff_cmd(base: Ms) -> Cmd {
    let k = draw_key();
    match nondet_range(11) {
        0 | 1 => Cmd::Set {
            key: k,
            val: draw_str(),
            cond: draw_set_cond(),
            ttl: match draw_ttl_offset() {
                None => {
                    if nondet_range(2) == 0 {
                        TtlArg::None
                    } else {
                        TtlArg::Keep
                    }
                }
                Some(off) => TtlArg::PxAt(base + off),
            },
            get: nondet_range(2) == 0,
        },
        2 => Cmd::Get { key: k },
        3 => Cmd::Del { key: k },
        4 => Cmd::Exists { key: k },
        5 => Cmd::Type { key: k },
        6 => Cmd::Expire {
            key: k,
            at: base + draw_ttl_offset().unwrap_or(FAR),
            cond: draw_expire_cond(),
        },
        7 => Cmd::Persist { key: k },
        8 => Cmd::Pttl { key: k },
        9 => Cmd::Keys,
        _ => Cmd::DbSize,
    }
}

/// Materialize a whole schedule up front, so both sides run the identical sequence.
///
/// Transactions are emitted as WELL-FORMED BLOCKS (`MULTI`, 1..QCAP commands, `EXEC` or
/// `DISCARD`) rather than as loose tokens. Random token streams mostly produce
/// "EXEC without MULTI" and rarely reach the interesting paths; blocks reach them every
/// time, and staying within QCAP keeps the model's bounded queue faithful. A few malformed
/// tokens are still emitted on purpose so the error replies are covered too.
///
/// Two clients, because WATCH/CAS is untestable with one: the whole point is that
/// somebody ELSE writes the key.
pub fn draw_schedule(seed: u64, base: Ms, len: usize) -> Vec<DiffStep> {
    begin(seed);
    let mut out = Vec::new();
    while out.len() < len {
        let c = (nondet_range(2)) as ClientId;
        match nondet_range(10) {
            0 => out.push(DiffStep::Tick),
            1 | 2 | 3 => {
                // A transaction block, often preceded by a WATCH, and often with the OTHER
                // client writing the watched key in between -- which is the only way the
                // CLIENT_DIRTY_CAS path is ever reached.
                let other: ClientId = 1 - c;
                let watched = draw_key();
                let watching = nondet_range(3) != 0;
                if watching {
                    out.push(DiffStep::Cmd(c, Cmd::Watch { client: c, key: watched }));
                    if nondet_range(2) == 0 {
                        out.push(DiffStep::Cmd(other, draw_write_on(watched, base)));
                    }
                }
                out.push(DiffStep::Cmd(c, Cmd::Multi));
                let n = 1 + nondet_range(QCAP as u64) as usize;
                for _ in 0..n {
                    out.push(DiffStep::Cmd(c, draw_txn_body(base)));
                }
                if nondet_range(8) == 0 {
                    out.push(DiffStep::Cmd(c, Cmd::BadCommand));
                }
                if nondet_range(6) == 0 {
                    out.push(DiffStep::Cmd(c, Cmd::Discard));
                } else {
                    out.push(DiffStep::Cmd(c, Cmd::Exec));
                }
            }
            6 => out.push(DiffStep::Cmd(c, Cmd::Watch { client: c, key: draw_key() })),
            4 => out.push(DiffStep::Cmd(c, Cmd::Unwatch { client: c })),
            // Deliberately malformed, to cover the error replies.
            5 if nondet_range(4) == 0 => out.push(DiffStep::Cmd(c, Cmd::Exec)),
            _ => out.push(DiffStep::Cmd(c, draw_diff_cmd(base))),
        }
    }
    out.truncate(len.max(1));
    out
}

// ------------------------------------------------------------------ rendering

/// Render a model command as the RESP argument vector to send.
fn render(cmd: &Cmd) -> Vec<Vec<u8>> {
    let s = |x: &str| x.as_bytes().to_vec();
    match cmd {
        Cmd::Set { key, val, cond, ttl, get } => {
            let mut a = vec![s("SET"), key_name(*key).into_bytes(), val_bytes(val)];
            match cond {
                SetCond::Always => {}
                SetCond::Nx => a.push(s("NX")),
                SetCond::Xx => a.push(s("XX")),
            }
            match ttl {
                TtlArg::None => {}
                TtlArg::Keep => a.push(s("KEEPTTL")),
                TtlArg::PxAt(at) => {
                    a.push(s("PXAT"));
                    a.push(at.to_string().into_bytes());
                }
            }
            if *get {
                a.push(s("GET"));
            }
            a
        }
        Cmd::Get { key } => vec![s("GET"), key_name(*key).into_bytes()],
        Cmd::Del { key } => vec![s("DEL"), key_name(*key).into_bytes()],
        Cmd::Exists { key } => vec![s("EXISTS"), key_name(*key).into_bytes()],
        Cmd::Type { key } => vec![s("TYPE"), key_name(*key).into_bytes()],
        Cmd::Expire { key, at, cond } => {
            let mut a = vec![s("PEXPIREAT"), key_name(*key).into_bytes(), at.to_string().into_bytes()];
            match cond {
                ExpireCond::None => {}
                ExpireCond::Nx => a.push(s("NX")),
                ExpireCond::Xx => a.push(s("XX")),
                ExpireCond::Gt => a.push(s("GT")),
                ExpireCond::Lt => a.push(s("LT")),
            }
            a
        }
        Cmd::Persist { key } => vec![s("PERSIST"), key_name(*key).into_bytes()],
        Cmd::Pttl { key } => vec![s("PTTL"), key_name(*key).into_bytes()],
        Cmd::Keys => vec![s("KEYS"), s("*")],
        Cmd::DbSize => vec![s("DBSIZE")],
        Cmd::Watch { key, .. } => vec![s("WATCH"), key_name(*key).into_bytes()],
        Cmd::Unwatch { .. } => vec![s("UNWATCH")],
        Cmd::Multi => vec![s("MULTI")],
        Cmd::Exec => vec![s("EXEC")],
        Cmd::Discard => vec![s("DISCARD")],
        // A command that is rejected at QUEUE time. Unknown commands are the cleanest way
        // to trigger CLIENT_DIRTY_EXEC without depending on arity rules.
        Cmd::BadCommand => vec![s("NOSUCHCOMMAND")],
    }
}

/// Does the server's reply agree with the model's?
///
/// PTTL is compared by BUCKET, not by value: the two clocks differ by however long the
/// schedule has been running, so the exact millisecond count will never match. The
/// meaningful content is -2 (no key) / -1 (no TTL) / >=0 (has TTL).
fn replies_agree(model: &Reply, server: &Resp, was_pttl: bool, pttl_at: &[usize]) -> bool {
    if was_pttl {
        let m = match model {
            Reply::Int(n) => *n,
            _ => return false,
        };
        let sv = match server.as_int() {
            Some(n) => n,
            None => return false,
        };
        let bucket = |n: i64| if n <= -2 { -2 } else if n == -1 { -1 } else { 0 };
        return bucket(m) == bucket(sv);
    }

    match (model, server) {
        (Reply::Ok, Resp::Simple(t)) => t == "OK",
        (Reply::Nil, r) => r.is_nil(),
        (Reply::Int(n), Resp::Int(m)) => n == m,
        (Reply::Str(s), Resp::Bulk(b)) => val_bytes(s) == *b,
        (Reply::Type(VType::Str), Resp::Simple(t)) => t == "string",
        (Reply::NoType, Resp::Simple(t)) => t == "none",
        (Reply::KeySet(set), Resp::Array(items)) => {
            let mut want: Vec<String> =
                (0..K).filter(|i| set[*i]).map(key_name).collect();
            let mut got: Vec<String> = items
                .iter()
                .filter_map(|r| r.as_bulk().map(|b| String::from_utf8_lossy(b).to_string()))
                .collect();
            want.sort();
            got.sort();
            want == got
        }
        (Reply::Error, Resp::Error(_)) => true,
        (Reply::Queued, Resp::Simple(t)) => t == "QUEUED",
        // -EXECABORT is an error reply; distinguish it from an ordinary one by prefix.
        (Reply::ExecAborted, Resp::Error(e)) => e.starts_with("EXECABORT"),
        (Reply::ExecNil, r) => r.is_nil(),
        (Reply::ExecArray(items), Resp::Array(got)) => {
            if items.len != got.len() {
                return false;
            }
            (0..items.len).all(|i| match items.items[i] {
                // `pttl_at` marks positions whose queued command was PTTL. Those must be
                // bucket-compared like a top-level PTTL: the model's clock and the
                // server's differ by however long the schedule has been running.
                Some(sub) => sub_agrees(&sub, &got[i], pttl_at.contains(&i)),
                None => false,
            })
        }
        _ => false,
    }
}

fn sub_agrees(model: &SubReply, server: &Resp, is_pttl: bool) -> bool {
    if is_pttl {
        let bucket = |n: i64| if n <= -2 { -2 } else if n == -1 { -1 } else { 0 };
        return match (model, server.as_int()) {
            (SubReply::Int(m), Some(sv)) => bucket(*m) == bucket(sv),
            _ => false,
        };
    }
    match (model, server) {
        (SubReply::Ok, Resp::Simple(t)) => t == "OK",
        (SubReply::Nil, r) => r.is_nil(),
        (SubReply::Int(n), Resp::Int(m)) => n == m,
        (SubReply::Str(s), Resp::Bulk(b)) => val_bytes(s) == *b,
        (SubReply::Type(VType::Str), Resp::Simple(t)) => t == "string",
        (SubReply::NoType, Resp::Simple(t)) => t == "none",
        (SubReply::KeySet(set), Resp::Array(items)) => {
            let mut want: Vec<String> = (0..K).filter(|i| set[*i]).map(key_name).collect();
            let mut got: Vec<String> = items
                .iter()
                .filter_map(|r| r.as_bulk().map(|b| String::from_utf8_lossy(b).to_string()))
                .collect();
            want.sort();
            got.sort();
            want == got
        }
        (SubReply::Error, Resp::Error(_)) => true,
        _ => false,
    }
}

// ------------------------------------------------------------------ snapshots

#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// PHYSICAL key count -- DBSIZE does not filter expired keys.
    pub dbsize: i64,
    /// LOGICALLY visible keys, from KEYS (which filters but does not delete).
    pub visible: Vec<String>,
    /// (key, value, has_ttl) for each visible key.
    pub values: Vec<(String, Vec<u8>, bool)>,
}

fn model_snapshot(w: &World) -> Snapshot {
    let dbsize = (0..K).filter(|i| w.slots[*i].present).count() as i64;
    let mut visible = Vec::new();
    let mut values = Vec::new();
    for i in 0..K {
        if w.slots[i].present && !crate::model::expire::key_is_expired(w, i) {
            visible.push(key_name(i));
            let Value::Str(s) = w.slots[i].value;
            values.push((key_name(i), val_bytes(&s), w.slots[i].has_ttl()));
        }
    }
    visible.sort();
    values.sort();
    Snapshot { dbsize, visible, values }
}

/// Read the server's snapshot WITHOUT mutating it.
///
/// Order matters: DBSIZE and KEYS are pure observations, so they come first. GET is only
/// issued for keys KEYS already reported live, so it cannot trigger a lazy delete and
/// perturb what we are measuring.
fn server_snapshot(c: &mut Client) -> std::io::Result<Snapshot> {
    let dbsize = c.cmd_s(&["DBSIZE"])?.as_int().unwrap_or(-1);
    let visible: Vec<String> = match c.cmd_s(&["KEYS", "*"])? {
        Resp::Array(items) => items
            .iter()
            .filter_map(|r| r.as_bulk().map(|b| String::from_utf8_lossy(b).to_string()))
            .collect(),
        _ => Vec::new(),
    };
    let mut visible_sorted = visible.clone();
    visible_sorted.sort();

    let mut values = Vec::new();
    for k in &visible_sorted {
        let v = match c.cmd_s(&["GET", k])? {
            Resp::Bulk(b) => b,
            _ => continue,
        };
        let ttl = c.cmd_s(&["PTTL", k])?.as_int().unwrap_or(-2);
        values.push((k.clone(), v, ttl >= 0));
    }
    values.sort();
    Ok(Snapshot { dbsize, visible: visible_sorted, values })
}

// ------------------------------------------------------------------ the run

pub struct Divergence {
    pub seed: u64,
    pub step: usize,
    pub what: String,
}

/// Which interesting paths a run actually reached.
///
/// Without this, "400/400 agreed" is not evidence: a schedule generator that never reaches
/// EXEC would agree perfectly and prove nothing about transactions.
#[derive(Default, Debug)]
pub struct Coverage {
    pub queued: u64,
    pub exec_ok: u64,
    pub exec_aborted: u64,
    pub exec_nil: u64,
    pub errors: u64,
    pub framed_units: u64,
    pub bare_units: u64,
}

impl Coverage {
    pub fn merge(&mut self, o: &Coverage) {
        self.queued += o.queued;
        self.exec_ok += o.exec_ok;
        self.exec_aborted += o.exec_aborted;
        self.exec_nil += o.exec_nil;
        self.errors += o.errors;
        self.framed_units += o.framed_units;
        self.bare_units += o.bare_units;
    }
}

/// Run one schedule on both sides. Returns the first divergence, if any.
pub fn run_schedule(
    conns: &mut [Client],
    seed: u64,
    base: Ms,
    sched: &[DiffStep],
    cov: &mut Coverage,
) -> std::io::Result<Option<Divergence>> {
    // Reset BOTH connections: a transaction or a WATCH left open by the previous schedule
    // would silently contaminate this one.
    for c in conns.iter_mut() {
        let _ = c.cmd_s(&["DISCARD"]);
        let _ = c.cmd_s(&["UNWATCH"]);
    }
    conns[0].cmd_s(&["FLUSHALL"])?;
    let mut w = World::empty(base);
    // Mirror of each client's queued commands, so an EXEC reply can be compared
    // position-by-position. The model's ExecResults deliberately does not record which
    // command produced each element.
    let mut queued: Vec<Vec<Cmd>> = vec![Vec::new(), Vec::new()];

    for (i, st) in sched.iter().enumerate() {
        match st {
            DiffStep::Tick => {
                std::thread::sleep(std::time::Duration::from_millis(TICK_MS as u64));
                w.clock += TICK_MS;
            }
            DiffStep::Cmd(cli, cmd) => {
                // PTTL inside a MULTI replies +QUEUED, so only treat it as a PTTL
                // comparison when it actually executes.
                let was_pttl = matches!(cmd, Cmd::Pttl { .. }) && !w.clients[*cli].in_multi;
                let m = dispatch(&mut w, *cli, *cmd);

                let pttl_at: Vec<usize> = if matches!(cmd, Cmd::Exec) {
                    queued[*cli].iter().enumerate()
                        .filter(|(_, q)| matches!(q, Cmd::Pttl { .. }))
                        .map(|(i, _)| i)
                        .collect()
                } else {
                    Vec::new()
                };
                match m {
                    Reply::Queued => queued[*cli].push(*cmd),
                    Reply::ExecArray(_) | Reply::ExecAborted | Reply::ExecNil => {
                        queued[*cli].clear()
                    }
                    _ => {
                        if matches!(cmd, Cmd::Discard | Cmd::Multi) {
                            queued[*cli].clear();
                        }
                    }
                }

                let args = render(cmd);
                let refs: Vec<&[u8]> = args.iter().map(|v| v.as_slice()).collect();
                match m {
                    Reply::Queued => cov.queued += 1,
                    Reply::ExecArray(_) => cov.exec_ok += 1,
                    Reply::ExecAborted => cov.exec_aborted += 1,
                    Reply::ExecNil => cov.exec_nil += 1,
                    Reply::Error => cov.errors += 1,
                    _ => {}
                }
                let s = conns[*cli].cmd(&refs)?;
                if !replies_agree(&m, &s, was_pttl, &pttl_at) {
                    return Ok(Some(Divergence {
                        seed,
                        step: i,
                        what: format!(
                            "reply mismatch on client {cli} for {}: model {:?} vs server {:?}",
                            String::from_utf8_lossy(
                                &args.iter().map(|a| String::from_utf8_lossy(a).to_string())
                                    .collect::<Vec<_>>().join(" ").into_bytes()
                            ),
                            describe(&m),
                            s
                        ),
                    }));
                }
            }
        }
    }

    // Close any block still open at the end of the schedule, so the snapshot reads state
    // rather than +QUEUED.
    for (i, c) in conns.iter_mut().enumerate() {
        if w.clients[i].in_multi {
            let _ = c.cmd_s(&["DISCARD"]);
            w.clients[i].reset_txn();
        }
    }

    {
        let mut i = 0;
        while i < w.repl.len {
            match w.repl.get(i) {
                Some(Effect::Multi) => cov.framed_units += 1,
                Some(Effect::Exec) => {}
                Some(_) => cov.bare_units += 1,
                None => {}
            }
            i += 1;
        }
    }

    let ms = model_snapshot(&w);
    let ss = server_snapshot(&mut conns[0])?;
    if ms != ss {
        return Ok(Some(Divergence {
            seed,
            step: sched.len(),
            what: format!("snapshot mismatch:\n    model  {ms:?}\n    server {ss:?}"),
        }));
    }
    Ok(None)
}

/// Human-readable form of a schedule step, for diagnosis.
pub fn describe_step(st: &DiffStep) -> String {
    match st {
        DiffStep::Tick => "         TICK".to_string(),
        DiffStep::Cmd(c, cmd) => {
            let args = render(cmd);
            let text: Vec<String> =
                args.iter().map(|a| String::from_utf8_lossy(a).to_string()).collect();
            format!("client {c}  {}", text.join(" "))
        }
    }
}

fn describe(r: &Reply) -> String {
    match r {
        Reply::Ok => "OK".into(),
        Reply::Nil => "nil".into(),
        Reply::Int(n) => format!("int({n})"),
        Reply::Str(s) => format!("bulk({:?})", val_bytes(s)),
        Reply::KeySet(set) => {
            format!("keys({:?})", (0..K).filter(|i| set[*i]).map(key_name).collect::<Vec<_>>())
        }
        Reply::Type(_) => "string".into(),
        Reply::NoType => "none".into(),
        Reply::Error => "error".into(),
        Reply::Queued => "QUEUED".into(),
        Reply::ExecAborted => "EXECABORT".into(),
        Reply::ExecNil => "exec-nil".into(),
        Reply::ExecArray(r) => format!("exec[{}]", r.len),
    }
}

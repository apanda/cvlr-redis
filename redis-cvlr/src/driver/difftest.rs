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
    Cmd(Cmd),
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
pub fn draw_schedule(seed: u64, base: Ms, len: usize) -> Vec<DiffStep> {
    begin(seed);
    (0..len)
        .map(|_| {
            if nondet_range(8) == 0 {
                DiffStep::Tick
            } else {
                DiffStep::Cmd(draw_diff_cmd(base))
            }
        })
        .collect()
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
        Cmd::Watch { .. } | Cmd::Unwatch { .. } => vec![s("PING")],
    }
}

/// Does the server's reply agree with the model's?
///
/// PTTL is compared by BUCKET, not by value: the two clocks differ by however long the
/// schedule has been running, so the exact millisecond count will never match. The
/// meaningful content is -2 (no key) / -1 (no TTL) / >=0 (has TTL).
fn replies_agree(model: &Reply, server: &Resp, was_pttl: bool) -> bool {
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

/// Run one schedule on both sides. Returns the first divergence, if any.
pub fn run_schedule(
    c: &mut Client,
    seed: u64,
    base: Ms,
    sched: &[DiffStep],
) -> std::io::Result<Option<Divergence>> {
    c.cmd_s(&["FLUSHALL"])?;
    let mut w = World::empty(base);

    for (i, st) in sched.iter().enumerate() {
        match st {
            DiffStep::Tick => {
                std::thread::sleep(std::time::Duration::from_millis(TICK_MS as u64));
                w.clock += TICK_MS;
            }
            DiffStep::Cmd(cmd) => {
                let was_pttl = matches!(cmd, Cmd::Pttl { .. });
                let m = exec_cmd(&mut w, 0, *cmd);
                let args = render(cmd);
                let refs: Vec<&[u8]> = args.iter().map(|v| v.as_slice()).collect();
                let s = c.cmd(&refs)?;
                if !replies_agree(&m, &s, was_pttl) {
                    return Ok(Some(Divergence {
                        seed,
                        step: i,
                        what: format!(
                            "reply mismatch for {}: model {:?} vs server {:?}",
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

    let ms = model_snapshot(&w);
    let ss = server_snapshot(c)?;
    if ms != ss {
        return Ok(Some(Divergence {
            seed,
            step: sched.len(),
            what: format!("snapshot mismatch:\n    model  {ms:?}\n    server {ss:?}"),
        }));
    }
    Ok(None)
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
    }
}

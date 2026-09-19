//! Differential test: the model against a real redis-server 8.9.241.
//!
//! This is the only evidence in the project that touches the shipped C. It samples
//! schedules rather than quantifying over them, so it can never replace the Prover -- but
//! it is what stops the model from being a second, wrong Redis.
//!
//! Requires `redis/src/redis-server` to be built. Skips (does not fail) if it is absent.
#![cfg(all(feature = "certora", feature = "rt"))]

use std::process::{Child, Command, Stdio};

use redis_cvlr::driver::difftest::*;
use redis_cvlr::driver::resp::{Client, Resp};
use redis_cvlr::model::cmd::*;
use redis_cvlr::model::step::dispatch;

const SERVER: &str = "../redis/src/redis-server";
const PORT: u16 = 7810;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `--enable-debug-command yes` is not optional: DEBUG is a protected command, and without
/// it `DEBUG SET-ACTIVE-EXPIRE 0` fails, the active cycle reaps test keys within ~100ms,
/// and every expiry comparison silently gets the wrong answer.
fn start(port: u16) -> Option<(Server, Client)> {
    if !std::path::Path::new(SERVER).exists() {
        return None;
    }
    let child = Command::new(SERVER)
        .args([
            "--port", &port.to_string(),
            "--save", "",
            "--appendonly", "no",
            "--enable-debug-command", "yes",
            "--databases", "16",
            "--maxmemory", "0",
            "--logfile", "",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let server = Server(child);

    for _ in 0..100 {
        if let Ok(mut c) = Client::connect(port) {
            if c.cmd_s(&["PING"]).is_ok() {
                return Some((server, c));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}

fn server_mstime(c: &mut Client) -> i64 {
    match c.cmd_s(&["TIME"]).unwrap() {
        Resp::Array(v) if v.len() == 2 => {
            let s: i64 = String::from_utf8_lossy(v[0].as_bulk().unwrap()).parse().unwrap();
            let us: i64 = String::from_utf8_lossy(v[1].as_bulk().unwrap()).parse().unwrap();
            s * 1000 + us / 1000
        }
        _ => 0,
    }
}

#[test]
fn model_matches_real_redis() {
    let Some((_srv, mut c)) = start(PORT) else {
        eprintln!("SKIP: {SERVER} not built -- run `make -C ../redis/src redis-server`");
        return;
    };

    // Disable active expiry so the ONLY deletions are the lazy ones the model performs.
    let r = c.cmd_s(&["DEBUG", "SET-ACTIVE-EXPIRE", "0"]).unwrap();
    assert!(
        matches!(r, Resp::Simple(ref s) if s == "OK"),
        "DEBUG SET-ACTIVE-EXPIRE failed ({r:?}) -- is --enable-debug-command set? \
         Without it every expiry comparison is wrong."
    );

    // Scale up for a stronger sample: DIFFTEST_SCHEDULES=5000 DIFFTEST_LEN=24 cargo test ...
    let schedules: u64 = std::env::var("DIFFTEST_SCHEDULES")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(400);
    let len: usize = std::env::var("DIFFTEST_LEN")
        .ok().and_then(|v| v.parse().ok()).unwrap_or(12);

    // Two clients: WATCH/CAS is meaningless with one, since the whole property is that
    // somebody ELSE wrote the key.
    let mut conns = vec![c, Client::connect(PORT).unwrap()];

    let mut checked = 0usize;
    let mut divergences = Vec::new();
    let mut cov = Coverage::default();

    for seed in 1..=schedules {
        let base = server_mstime(&mut conns[0]);
        let sched = draw_schedule(seed, base, len);
        let mut c1 = Coverage::default();
        let r = run_schedule(&mut conns, seed, base, &sched, &mut c1).unwrap();
        cov.merge(&c1);
        match r {
            None => checked += 1,
            Some(d) => {
                if divergences.len() < 5 {
                    divergences.push(format!("seed {} step {}: {}", d.seed, d.step, d.what));
                }
            }
        }
    }

    println!("\n{checked}/{schedules} schedules agreed ({len} steps each)");
    println!("coverage: {cov:?}");
    // A generator that never reaches EXEC would agree perfectly and prove nothing.
    assert!(cov.exec_ok > 0, "no transaction ever committed -- schedules are not exercising EXEC");
    assert!(cov.exec_aborted > 0, "EXECABORT path never reached");
    assert!(cov.exec_nil > 0, "WATCH-invalidated EXEC path never reached");
    assert!(cov.framed_units > 0, "no multi-op unit was ever MULTI/EXEC framed");
    if !divergences.is_empty() {
        println!("\n{} diverged; first few:", schedules as usize - checked);
        for d in &divergences {
            println!("  {d}");
        }
    }
    assert!(divergences.is_empty(), "model diverged from real redis-server");
}

/// A harness that never fails is proving nothing. This injects a known model bug and
/// asserts the harness catches it.
#[test]
fn harness_detects_an_injected_bug() {
    let Some((_srv, mut c)) = start(PORT + 1) else {
        eprintln!("SKIP: {SERVER} not built");
        return;
    };
    c.cmd_s(&["DEBUG", "SET-ACTIVE-EXPIRE", "0"]).unwrap();

    // The classic wrong model: DBSIZE filters expired keys. It does not (db.c:3148-3173).
    //
    // NOTE the setup must use a RELATIVE PX and then let it elapse. `SET k v PXAT <past>`
    // would leave no key at all (t_string.c:161-175) -- which is itself a bug this
    // harness caught in the model.
    c.cmd_s(&["FLUSHALL"]).unwrap();
    c.cmd_s(&["SET", "k0", "v", "PX", "50"]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));

    let dbsize = c.cmd_s(&["DBSIZE"]).unwrap().as_int().unwrap();
    let keys = match c.cmd_s(&["KEYS", "*"]).unwrap() {
        Resp::Array(v) => v.len(),
        _ => 0,
    };

    assert_eq!(dbsize, 1, "DBSIZE must count the physically present, logically expired key");
    assert_eq!(keys, 0, "KEYS must filter it");
    println!("\ninjected-bug check: a model with DBSIZE filtering expired keys would report \
              0 here; the server reports {dbsize}. The harness can tell them apart.");
}

/// FINDINGS.md F-01 reachability probe.
///
/// The `break` at multi.c:415 needs `wk->expired == 1` on a watcher WHILE the key is
/// physically present. Ordinary write paths cannot produce that: they lazy-expire first,
/// which calls `touchWatchedKey` with the key absent and clears the flag (multi.c:413).
///
/// `touchAllWatchedKeysInDb` is the one place that SETS the flag without a deletion
/// (multi.c:472-474): "Non-existing key is replaced with an expired key." That is the
/// SWAPDB path. This probe tries it.
///
/// Reports; does not assert. A negative result is a real result -- see FINDINGS.md.
#[test]
fn f01_swapdb_reachability_probe() {
    let Some((_srv, mut c)) = start(PORT + 2) else {
        eprintln!("SKIP: {SERVER} not built");
        return;
    };
    c.cmd_s(&["DEBUG", "SET-ACTIVE-EXPIRE", "0"]).unwrap();
    c.cmd_s(&["FLUSHALL"]).unwrap();

    let mut a = Client::connect(PORT + 2).unwrap(); // the expired-at-WATCH watcher
    let mut b = Client::connect(PORT + 2).unwrap(); // the watcher that must be dirtied

    // db1 gets a key that will be logically expired but physically present.
    c.cmd_s(&["SELECT", "1"]).unwrap();
    c.cmd_s(&["SET", "wk", "v", "PX", "50"]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));
    let db1_size = c.cmd_s(&["DBSIZE"]).unwrap().as_int().unwrap();

    // A watches 'wk' in db0, where it does NOT exist -> wk->expired starts 0.
    a.cmd_s(&["SELECT", "0"]).unwrap();
    a.cmd_s(&["WATCH", "wk"]).unwrap();

    // SWAPDB brings the expired-but-present key into db0. touchAllWatchedKeysInDb should
    // take the `!exists_in_emptied && keyIsExpired(replaced_with)` arm and set A's
    // wk->expired = 1 -- with the key PRESENT.
    let swap = c.cmd_s(&["SWAPDB", "0", "1"]).unwrap();

    // B now watches the same key in db0. It is logically expired, so B also gets
    // expired = 1 -- but B is appended AFTER A.
    b.cmd_s(&["SELECT", "0"]).unwrap();
    b.cmd_s(&["WATCH", "wk"]).unwrap();

    // Revive the key WITHOUT a lazy-expire delete would be ideal; a plain SET will
    // lazy-expire first. Try it anyway and see which way it goes.
    c.cmd_s(&["SELECT", "0"]).unwrap();
    c.cmd_s(&["SET", "wk", "fresh"]).unwrap();
    c.cmd_s(&["SET", "wk", "fresher"]).unwrap();

    b.cmd_s(&["MULTI"]).unwrap();
    b.cmd_s(&["GET", "wk"]).unwrap();
    let b_exec = b.cmd_s(&["EXEC"]).unwrap();

    a.cmd_s(&["MULTI"]).unwrap();
    a.cmd_s(&["GET", "wk"]).unwrap();
    let a_exec = a.cmd_s(&["EXEC"]).unwrap();

    println!("\nF-01 SWAPDB reachability probe");
    println!("  db1 size before swap : {db1_size} (1 = expired key physically present)");
    println!("  SWAPDB               : {swap:?}");
    println!("  B EXEC               : {b_exec:?}");
    println!("  A EXEC               : {a_exec:?}");
    if b_exec.is_nil() {
        println!("  => B was invalidated. The break did NOT skip it on this path.");
        println!("     F-01 remains NOT REPRODUCED.");
    } else {
        println!("  => *** B's EXEC SUCCEEDED despite wk being overwritten after B WATCHed it.");
        println!("     This would be a client-visible WATCH/EXEC soundness bug. Re-verify by");
        println!("     hand before reporting -- see FINDINGS.md F-01.");
    }
}

/// Render the model's replication log as a sequence of command NAMES with framing.
///
/// Names and framing, not full arguments, deliberately: `DEL` vs `UNLINK` depends on the
/// lazyfree config, and Redis rewrites arguments (relative TTLs become absolute `PXAT`).
/// What P-13 is about is the FRAMING and the op count, and comparing shape rather than
/// bytes tests exactly that without coupling to rewriting details.
fn model_stream(w: &redis_cvlr::model::state::World) -> Vec<String> {
    use redis_cvlr::model::state::Effect;
    let mut out = Vec::new();
    for i in 0..w.repl.len {
        out.push(match w.repl.get(i) {
            Some(Effect::Multi) => "MULTI".to_string(),
            Some(Effect::Exec) => "EXEC".to_string(),
            Some(Effect::Set { .. }) => "SET".to_string(),
            Some(Effect::Del { .. }) => "DEL".to_string(),
            Some(Effect::PExpireAt { .. }) => "PEXPIREAT".to_string(),
            Some(Effect::Persist { .. }) => "PERSIST".to_string(),
            None => continue,
        });
    }
    out
}

fn server_stream(cmds: &[Vec<String>]) -> Vec<String> {
    cmds.iter()
        .map(|c| {
            let n = c[0].to_uppercase();
            // DEL and UNLINK are the same effect; which one appears depends on
            // lazyfree-lazy-* config, not on semantics.
            if n == "UNLINK" { "DEL".to_string() } else { n }
        })
        .collect()
}

/// P-13: a unit is MULTI/EXEC framed iff it emitted more than one op.
///
/// This is the headline atomicity property, and it is the one that most needs a real
/// oracle rather than a model-only check -- see FINDINGS.md D-01 for why.
#[test]
fn propagation_framing_matches_real_redis() {
    let Some((_srv, mut c)) = start(PORT + 3) else {
        eprintln!("SKIP: {SERVER} not built");
        return;
    };
    c.cmd_s(&["DEBUG", "SET-ACTIVE-EXPIRE", "0"]).unwrap();
    c.cmd_s(&["FLUSHALL"]).unwrap();

    let mut repl = Client::connect(PORT + 3).unwrap();
    let rdb = repl.sync_start().unwrap();
    println!("\nattached as replica (RDB payload {rdb} bytes)");

    let base = server_mstime(&mut c);

    struct Case {
        name: &'static str,
        run: fn(&mut Client, i64),
        model: fn(&mut redis_cvlr::model::state::World, i64),
    }

    let cases = [
        Case {
            name: "single write -> bare, no framing",
            run: |c, _| {
                c.cmd_s(&["SET", "k0", "v"]).unwrap();
            },
            model: |w, _| {
                dispatch(w, 0, Cmd::Set { key: 0, val: sval(), cond: SetCond::Always,
                                          ttl: TtlArg::None, get: false });
            },
        },
        Case {
            name: "transaction with ONE write -> still bare",
            run: |c, _| {
                c.cmd_s(&["MULTI"]).unwrap();
                c.cmd_s(&["SET", "k0", "v"]).unwrap();
                c.cmd_s(&["EXEC"]).unwrap();
            },
            model: |w, _| {
                dispatch(w, 0, Cmd::Multi);
                dispatch(w, 0, Cmd::Set { key: 0, val: sval(), cond: SetCond::Always,
                                          ttl: TtlArg::None, get: false });
                dispatch(w, 0, Cmd::Exec);
            },
        },
        Case {
            name: "transaction with TWO writes -> MULTI/EXEC framed",
            run: |c, _| {
                c.cmd_s(&["MULTI"]).unwrap();
                c.cmd_s(&["SET", "k0", "v"]).unwrap();
                c.cmd_s(&["SET", "k1", "v"]).unwrap();
                c.cmd_s(&["EXEC"]).unwrap();
            },
            model: |w, _| {
                dispatch(w, 0, Cmd::Multi);
                dispatch(w, 0, Cmd::Set { key: 0, val: sval(), cond: SetCond::Always,
                                          ttl: TtlArg::None, get: false });
                dispatch(w, 0, Cmd::Set { key: 1, val: sval(), cond: SetCond::Always,
                                          ttl: TtlArg::None, get: false });
                dispatch(w, 0, Cmd::Exec);
            },
        },
        Case {
            name: "transaction with NO write -> propagates nothing",
            run: |c, _| {
                c.cmd_s(&["MULTI"]).unwrap();
                c.cmd_s(&["GET", "k0"]).unwrap();
                c.cmd_s(&["EXEC"]).unwrap();
            },
            model: |w, _| {
                dispatch(w, 0, Cmd::Multi);
                dispatch(w, 0, Cmd::Get { key: 0 });
                dispatch(w, 0, Cmd::Exec);
            },
        },
    ];

    let mut failures = Vec::new();
    for case in &cases {
        c.cmd_s(&["FLUSHALL"]).unwrap();
        let _ = repl.drain_propagated(120).unwrap(); // discard the FLUSHALL

        let mut w = redis_cvlr::model::state::World::empty(base);
        (case.model)(&mut w, base);
        (case.run)(&mut c, base);

        let got = server_stream(&repl.drain_propagated(180).unwrap());
        let want = model_stream(&w);
        let ok = got == want;
        println!("  {:<48} model {:?}  server {:?}  {}",
                 case.name, want, got, if ok { "ok" } else { "MISMATCH" });
        if !ok {
            failures.push(case.name);
        }
    }

    assert!(failures.is_empty(), "propagation framing diverged: {failures:?}");
}

fn sval() -> redis_cvlr::model::state::Str {
    let mut s = redis_cvlr::model::state::Str::empty();
    s.bytes[0] = b'v';
    s.len = 1;
    s
}

/// A unit that lazily expires a key AND writes emits TWO ops, so by the framing rule it
/// must be MULTI/EXEC wrapped -- even though it is a single client command.
///
/// This is the prediction that falls out of routing lazy-expire DELs through the same
/// pending buffer as everything else. It also sharpens P-09: the bare `DEL` that
/// tests/unit/expire.tcl:809 pins is not a special case for expiry, it is just what a
/// ONE-op unit looks like.
#[test]
fn lazy_expire_framing_matches_real_redis() {
    let Some((_srv, mut c)) = start(PORT + 4) else {
        eprintln!("SKIP: {SERVER} not built");
        return;
    };
    c.cmd_s(&["DEBUG", "SET-ACTIVE-EXPIRE", "0"]).unwrap();
    c.cmd_s(&["FLUSHALL"]).unwrap();
    let mut repl = Client::connect(PORT + 4).unwrap();
    repl.sync_start().unwrap();

    // --- case A: a READ that lazily expires -> one op, bare DEL
    c.cmd_s(&["SET", "ka", "v", "PX", "50"]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));
    let _ = repl.drain_propagated(150).unwrap(); // discard setup
    c.cmd_s(&["GET", "ka"]).unwrap();
    let read_stream = server_stream(&repl.drain_propagated(200).unwrap());

    // --- case B: a WRITE onto a logically expired key -> expire DEL + the write
    c.cmd_s(&["SET", "kb", "v", "PX", "50"]).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));
    let _ = repl.drain_propagated(150).unwrap(); // discard setup
    c.cmd_s(&["SET", "kb", "fresh"]).unwrap();
    let write_stream = server_stream(&repl.drain_propagated(200).unwrap());

    println!("\n  read  that lazily expires : {read_stream:?}");
    println!("  write onto an expired key : {write_stream:?}");

    assert_eq!(read_stream, vec!["DEL"], "a lazy-expiring READ should propagate a bare DEL");

    // The model predicts MULTI DEL SET EXEC. Report either way rather than assuming.
    let framed = write_stream.first().map(|s| s == "MULTI").unwrap_or(false);
    if framed {
        println!("  => framed, as the model predicts: a 2-op unit is wrapped.");
        assert_eq!(write_stream, vec!["MULTI", "DEL", "SET", "EXEC"]);
    } else {
        println!("  => NOT framed. The model over-predicts here; the write path must");
        println!("     suppress or merge the expire DEL. Investigate before trusting P-13.");
    }
}

/// Replay one seed with the schedule printed, for diagnosing a divergence.
/// `DIFFTEST_SEED=26 cargo test --features certora,rt --release --test difftest replay -- --nocapture`
#[test]
fn replay_one_seed() {
    let Ok(seed_s) = std::env::var("DIFFTEST_SEED") else {
        eprintln!("SKIP: set DIFFTEST_SEED to replay a schedule");
        return;
    };
    let seed: u64 = seed_s.parse().unwrap();
    let len: usize = std::env::var("DIFFTEST_LEN").ok().and_then(|v| v.parse().ok()).unwrap_or(28);

    let Some((_srv, c)) = start(PORT + 5) else {
        eprintln!("SKIP: {SERVER} not built");
        return;
    };
    let mut conns = vec![c, Client::connect(PORT + 5).unwrap()];
    conns[0].cmd_s(&["DEBUG", "SET-ACTIVE-EXPIRE", "0"]).unwrap();

    let base = server_mstime(&mut conns[0]);
    let sched = draw_schedule(seed, base, len);

    println!("\nseed {seed}, {} steps:", sched.len());
    for (i, st) in sched.iter().enumerate() {
        println!("  {i:>3}  {}", describe_step(st));
    }

    let mut cov = Coverage::default();
    match run_schedule(&mut conns, seed, base, &sched, &mut cov).unwrap() {
        None => println!("\n  agreed"),
        Some(d) => println!("\n  DIVERGED at step {}: {}", d.step, d.what),
    }
}

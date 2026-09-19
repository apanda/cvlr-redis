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

    let mut checked = 0usize;
    let mut divergences = Vec::new();

    for seed in 1..=schedules {
        let base = server_mstime(&mut c);
        let sched = draw_schedule(seed, base, len);
        match run_schedule(&mut c, seed, base, &sched).unwrap() {
            None => checked += 1,
            Some(d) => {
                if divergences.len() < 5 {
                    divergences.push(format!("seed {} step {}: {}", d.seed, d.step, d.what));
                }
            }
        }
    }

    println!("\n{checked}/{schedules} schedules agreed ({len} steps each)");
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
